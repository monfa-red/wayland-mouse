//! `tune` — a live terminal UI (ratatui) for shaping the pointer and wheel
//! curves while you move the mouse. It talks to the running daemon over the
//! control socket: every edit is pushed live (you feel it instantly) and the
//! daemon streams back the measured speed/gain so a marker rides the curve.
//!
//! Everything here is config-space (the values as they appear in the file,
//! independent of DPI); the daemon does the DPI rescale internally.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Block, BorderType, Chart, Dataset, GraphType, Paragraph, Tabs, Wrap};
use ratatui::{DefaultTerminal, Frame};

use crate::config::{self, ConfigFile, Settings, REFERENCE_DPI};
use crate::ipc::{Request, Response, TelemetrySample, SOCKET_PATH};

// Palette
const ACCENT: Color = Color::Cyan;
const CURVE: Color = Color::LightGreen;
const MARKER: Color = Color::LightRed;
const BAR_FILL: Color = Color::Cyan;
const BAR_EMPTY: Color = Color::DarkGray;
const KEY: Color = Color::Yellow;

// ---------------------------------------------------------------------------
// Socket client
// ---------------------------------------------------------------------------

struct Client {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
}

impl Client {
    fn connect() -> Result<Self, String> {
        let stream = UnixStream::connect(SOCKET_PATH).map_err(|e| e.to_string())?;
        let reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
        Ok(Client {
            writer: stream,
            reader,
        })
    }

    fn call(&mut self, req: &Request) -> Result<Response, String> {
        let mut line = serde_json::to_string(req).map_err(|e| e.to_string())?;
        line.push('\n');
        self.writer
            .write_all(line.as_bytes())
            .map_err(|e| e.to_string())?;
        let mut resp = String::new();
        let n = self
            .reader
            .read_line(&mut resp)
            .map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("daemon closed the connection".into());
        }
        serde_json::from_str(&resp).map_err(|e| e.to_string())
    }

    fn get_config(&mut self) -> Result<ConfigFile, String> {
        match self.call(&Request::GetConfig)? {
            Response::Config { config } => Ok(*config),
            Response::Error { message } => Err(message),
            _ => Err("unexpected response".into()),
        }
    }

    fn set_config(&mut self, cfg: &ConfigFile) -> Result<(), String> {
        match self.call(&Request::SetConfig {
            config: Box::new(cfg.clone()),
        })? {
            Response::Ok => Ok(()),
            Response::Error { message } => Err(message),
            _ => Err("unexpected response".into()),
        }
    }

    fn save(&mut self) -> Result<(), String> {
        match self.call(&Request::Save)? {
            Response::Ok => Ok(()),
            Response::Error { message } => Err(message),
            _ => Err("unexpected response".into()),
        }
    }

    fn telemetry(&mut self) -> Result<TelemetrySample, String> {
        match self.call(&Request::GetTelemetry)? {
            Response::Telemetry(t) => Ok(t),
            Response::Error { message } => Err(message),
            _ => Err("unexpected response".into()),
        }
    }
}

// ---------------------------------------------------------------------------
// Editable fields
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Field {
    PtrEnabled,
    Precision,
    MaxGain,
    Midpoint,
    Width,
    PtrSmoothing,
    WheelEnabled,
    StartSpeed,
    Strength,
    Curve,
    MaxMult,
    SmoothUp,
    SmoothDown,
    ResetMs,
    Dpi,
}

const PTR_FIELDS: [Field; 6] = [
    Field::PtrEnabled,
    Field::Precision,
    Field::MaxGain,
    Field::Midpoint,
    Field::Width,
    Field::PtrSmoothing,
];
const WHEEL_FIELDS: [Field; 8] = [
    Field::WheelEnabled,
    Field::StartSpeed,
    Field::Strength,
    Field::Curve,
    Field::MaxMult,
    Field::SmoothUp,
    Field::SmoothDown,
    Field::ResetMs,
];

impl Field {
    fn label(self) -> &'static str {
        match self {
            Field::PtrEnabled => "Pointer accel",
            Field::Precision => "Precision (slow)",
            Field::MaxGain => "Reach (fast)",
            Field::Midpoint => "Knee speed",
            Field::Width => "Transition width",
            Field::PtrSmoothing => "Smoothing",
            Field::WheelEnabled => "Wheel accel",
            Field::StartSpeed => "Start speed",
            Field::Strength => "Strength",
            Field::Curve => "Curve",
            Field::MaxMult => "Max multiplier",
            Field::SmoothUp => "Speed-up smooth",
            Field::SmoothDown => "Slow-down smooth",
            Field::ResetMs => "Reset pause",
            Field::Dpi => "Mouse DPI",
        }
    }

    fn is_bool(self) -> bool {
        matches!(self, Field::PtrEnabled | Field::WheelEnabled)
    }

    fn bounds(self) -> (f64, f64) {
        match self {
            Field::Precision => (0.1, 3.0),
            Field::MaxGain => (0.5, 8.0),
            Field::Midpoint => (200.0, 12000.0),
            Field::Width => (100.0, 8000.0),
            Field::PtrSmoothing => (1.0, 60.0),
            Field::StartSpeed => (0.0, 40.0),
            Field::Strength => (0.0, 1.0),
            Field::Curve => (0.5, 2.5),
            Field::MaxMult => (1.0, 20.0),
            Field::SmoothUp | Field::SmoothDown => (0.05, 1.0),
            Field::ResetMs => (40.0, 600.0),
            Field::Dpi => (200.0, 6400.0),
            Field::PtrEnabled | Field::WheelEnabled => (0.0, 1.0),
        }
    }

    fn step(self) -> f64 {
        match self {
            Field::Precision => 0.05,
            Field::MaxGain => 0.1,
            Field::Midpoint => 250.0,
            Field::Width => 250.0,
            Field::PtrSmoothing => 1.0,
            Field::StartSpeed => 1.0,
            Field::Strength => 0.01,
            Field::Curve => 0.05,
            Field::MaxMult => 0.5,
            Field::SmoothUp | Field::SmoothDown => 0.05,
            Field::ResetMs => 10.0,
            Field::Dpi => 50.0,
            Field::PtrEnabled | Field::WheelEnabled => 1.0,
        }
    }

    fn decimals(self) -> usize {
        match self {
            Field::Precision
            | Field::MaxGain
            | Field::Strength
            | Field::Curve
            | Field::SmoothUp
            | Field::SmoothDown => 2,
            Field::MaxMult => 1,
            _ => 0,
        }
    }

    fn unit(self) -> &'static str {
        match self {
            Field::Midpoint | Field::Width => " cnt/s",
            Field::StartSpeed => " det/s",
            Field::PtrSmoothing | Field::ResetMs => " ms",
            Field::MaxGain => "×",
            Field::MaxMult => "×",
            _ => "",
        }
    }
}

/// Effective config-space value of a field (bools as 0.0/1.0).
fn field_value(cfg: &ConfigFile, f: Field) -> f64 {
    let s = cfg.resolve_unscaled();
    match f {
        Field::PtrEnabled => s.pointer_accel as i32 as f64,
        Field::Precision => s.ptr_base,
        Field::MaxGain => s.ptr_max,
        Field::Midpoint => s.ptr_mid,
        Field::Width => s.ptr_width,
        Field::PtrSmoothing => s.ptr_tau * 1000.0,
        Field::WheelEnabled => s.wheel_enabled as i32 as f64,
        Field::StartSpeed => s.threshold_dps,
        Field::Strength => s.accel,
        Field::Curve => s.exponent,
        Field::MaxMult => s.max_mult,
        Field::SmoothUp => s.attack,
        Field::SmoothDown => s.release,
        Field::ResetMs => s.reset_gap.as_secs_f64() * 1000.0,
        Field::Dpi => s.dpi,
    }
}

fn field_set(cfg: &mut ConfigFile, f: Field, v: f64) {
    match f {
        Field::Precision => cfg.pointer.precision_gain = Some(v),
        Field::MaxGain => cfg.pointer.max_gain = Some(v),
        Field::Midpoint => cfg.pointer.midpoint_speed = Some(v),
        Field::Width => cfg.pointer.transition_width = Some(v),
        Field::PtrSmoothing => cfg.pointer.smoothing_ms = Some(v),
        Field::StartSpeed => cfg.wheel.start_speed = Some(v),
        Field::Strength => cfg.wheel.strength = Some(v),
        Field::Curve => cfg.wheel.curve = Some(v),
        Field::MaxMult => cfg.wheel.max_multiplier = Some(v),
        Field::SmoothUp => cfg.wheel.smoothing_up = Some(v),
        Field::SmoothDown => cfg.wheel.smoothing_down = Some(v),
        Field::ResetMs => cfg.wheel.reset_after_ms = Some(v),
        Field::Dpi => cfg.dpi = Some(v),
        Field::PtrEnabled | Field::WheelEnabled => {}
    }
}

/// Adjust a field by `dir` steps (snapping to the step grid), or toggle a bool.
fn field_adjust(cfg: &mut ConfigFile, f: Field, dir: f64) {
    if f.is_bool() {
        let on = field_value(cfg, f) > 0.5;
        match f {
            Field::PtrEnabled => cfg.pointer.enabled = Some(!on),
            Field::WheelEnabled => cfg.wheel.enabled = Some(!on),
            _ => {}
        }
        return;
    }
    let (lo, hi) = f.bounds();
    let step = f.step();
    let raw = field_value(cfg, f) + dir * step;
    let snapped = (raw / step).round() * step;
    field_set(cfg, f, snapped.clamp(lo, hi));
}

fn cycle_preset(cfg: &mut ConfigFile, dir: i32) {
    let names = config::PRESET_NAMES;
    let cur = names
        .iter()
        .position(|n| n.eq_ignore_ascii_case(&cfg.preset))
        .unwrap_or(0) as i32;
    let n = names.len() as i32;
    let next = (((cur + dir) % n) + n) % n;
    cfg.preset = names[next as usize].to_string();
}

// ---------------------------------------------------------------------------
// Curve math (mirrors pointer.rs / wheel.rs, config-space)
// ---------------------------------------------------------------------------

fn pointer_gain(s: &Settings, v: f64) -> f64 {
    let sig = 1.0 / (1.0 + (-(v - s.ptr_mid) / s.ptr_width).exp());
    s.ptr_base + (s.ptr_max - s.ptr_base) * sig
}

fn wheel_mult(s: &Settings, dps: f64) -> f64 {
    let over = dps - s.threshold_dps;
    if over <= 0.0 {
        1.0
    } else {
        (1.0 + s.accel * over.powf(s.exponent)).min(s.max_mult)
    }
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Pointer,
    Wheel,
    Buttons,
    General,
}

impl Tab {
    const ALL: [Tab; 4] = [Tab::Pointer, Tab::Wheel, Tab::Buttons, Tab::General];
    fn index(self) -> usize {
        Tab::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }
    fn title(self) -> &'static str {
        match self {
            Tab::Pointer => "Pointer",
            Tab::Wheel => "Wheel",
            Tab::Buttons => "Buttons",
            Tab::General => "General",
        }
    }
}

/// A selectable row on the Pointer/Wheel/General tabs.
enum Row {
    Knob(Field),
    Preset,
}

struct App {
    client: Client,
    cfg: ConfigFile,
    tab: Tab,
    sel: usize,
    tel: TelemetrySample,
    dirty: bool,
    status: String,
    confirm_quit: bool,
    quit: bool,
}

impl App {
    fn new(client: Client, cfg: ConfigFile) -> Self {
        App {
            client,
            cfg,
            tab: Tab::Pointer,
            sel: 0,
            tel: TelemetrySample::default(),
            dirty: false,
            status: "move + scroll the mouse to see the marker ride the curve".into(),
            confirm_quit: false,
            quit: false,
        }
    }

    fn rows(&self) -> Vec<Row> {
        match self.tab {
            Tab::Pointer => PTR_FIELDS.iter().map(|f| Row::Knob(*f)).collect(),
            Tab::Wheel => WHEEL_FIELDS.iter().map(|f| Row::Knob(*f)).collect(),
            Tab::General => vec![Row::Preset, Row::Knob(Field::Dpi)],
            Tab::Buttons => Vec::new(),
        }
    }

    fn row_count(&self) -> usize {
        match self.tab {
            Tab::Buttons => self.cfg.button.len(),
            _ => self.rows().len(),
        }
    }

    fn run(&mut self, terminal: &mut DefaultTerminal) -> std::io::Result<()> {
        while !self.quit {
            match self.client.telemetry() {
                Ok(t) => self.tel = t,
                Err(e) => self.status = format!("disconnected: {e}"),
            }
            terminal.draw(|f| ui(f, self))?;
            if event::poll(Duration::from_millis(33))? {
                if let Event::Key(k) = event::read()? {
                    if k.kind != KeyEventKind::Release {
                        self.on_key(k.code, k.modifiers);
                    }
                }
            }
        }
        Ok(())
    }

    fn on_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        let big = mods.contains(KeyModifiers::SHIFT);
        // Any non-quit key cancels a pending quit confirmation.
        if !matches!(code, KeyCode::Char('q') | KeyCode::Esc) {
            self.confirm_quit = false;
        }
        match code {
            KeyCode::Char('q') | KeyCode::Esc => {
                if self.dirty && !self.confirm_quit {
                    self.confirm_quit = true;
                    self.status =
                        "unsaved changes — press s to save, or q/Esc again to quit".into();
                } else {
                    self.quit = true;
                }
            }
            KeyCode::Tab => self.switch_tab(1),
            KeyCode::BackTab => self.switch_tab(-1),
            KeyCode::Char(c @ '1'..='4') => {
                self.tab = Tab::ALL[c as usize - '1' as usize];
                self.sel = 0;
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_sel(1),
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('-') => self.adjust(-1.0, big),
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char('+') | KeyCode::Char('=') => {
                self.adjust(1.0, big)
            }
            KeyCode::Char(' ') | KeyCode::Enter => self.toggle(),
            KeyCode::Char('p') => {
                cycle_preset(&mut self.cfg, 1);
                self.touch("preset changed");
            }
            KeyCode::Char('s') => self.do_save(),
            KeyCode::Char('r') => self.do_reset(),
            KeyCode::Char('d') if self.tab == Tab::Buttons => self.delete_button(),
            _ => {}
        }
    }

    fn switch_tab(&mut self, dir: i32) {
        let i = self.tab.index() as i32 + dir;
        let n = Tab::ALL.len() as i32;
        self.tab = Tab::ALL[(((i % n) + n) % n) as usize];
        self.sel = 0;
    }

    fn move_sel(&mut self, dir: i32) {
        let count = self.row_count();
        if count == 0 {
            return;
        }
        let i = self.sel as i32 + dir;
        self.sel = i.clamp(0, count as i32 - 1) as usize;
    }

    fn adjust(&mut self, dir: f64, big: bool) {
        if self.tab == Tab::Buttons {
            return;
        }
        let rows = self.rows();
        let Some(row) = rows.get(self.sel) else {
            return;
        };
        match row {
            Row::Preset => cycle_preset(&mut self.cfg, dir as i32),
            Row::Knob(f) => {
                if f.is_bool() {
                    field_adjust(&mut self.cfg, *f, dir);
                } else {
                    field_adjust(&mut self.cfg, *f, dir * if big { 5.0 } else { 1.0 });
                }
            }
        }
        self.touch("");
    }

    fn toggle(&mut self) {
        if self.tab == Tab::Buttons {
            return;
        }
        let rows = self.rows();
        let Some(row) = rows.get(self.sel) else {
            return;
        };
        match row {
            Row::Preset => cycle_preset(&mut self.cfg, 1),
            Row::Knob(f) if f.is_bool() => field_adjust(&mut self.cfg, *f, 1.0),
            Row::Knob(_) => return,
        }
        self.touch("");
    }

    fn do_reset(&mut self) {
        match self.tab {
            Tab::Pointer => self.cfg.pointer = Default::default(),
            Tab::Wheel => self.cfg.wheel = Default::default(),
            Tab::General => self.cfg.dpi = None,
            Tab::Buttons => return,
        }
        self.touch("reset this tab to the preset");
    }

    fn delete_button(&mut self) {
        if self.sel < self.cfg.button.len() {
            let removed = self.cfg.button.remove(self.sel);
            self.sel = self.sel.min(self.cfg.button.len().saturating_sub(1));
            self.touch(&format!("removed {} — restart to apply", removed.match_));
        }
    }

    /// Mark dirty and push the live config to the daemon.
    fn touch(&mut self, msg: &str) {
        self.dirty = true;
        if let Err(e) = self.client.set_config(&self.cfg) {
            self.status = format!("live-apply failed: {e}");
        } else if !msg.is_empty() {
            self.status = msg.to_string();
        }
    }

    fn do_save(&mut self) {
        match self.client.save() {
            Ok(()) => {
                self.dirty = false;
                self.confirm_quit = false;
                self.status = "saved to /etc/wayland-mouse/config.toml ✓".into();
            }
            Err(e) => self.status = format!("save failed: {e}"),
        }
    }
}

/// Entry point for the `tune` subcommand.
pub fn run() -> i32 {
    let mut client = match Client::connect() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("wayland-mouse tune: can't reach the daemon at {SOCKET_PATH}: {e}");
            eprintln!("Is the service running, and are you root?");
            eprintln!("  sudo systemctl start wayland-mouse  &&  sudo wayland-mouse tune");
            return 1;
        }
    };
    let cfg = match client.get_config() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("wayland-mouse tune: {e}");
            return 1;
        }
    };

    let mut terminal = ratatui::init();
    let mut app = App::new(client, cfg);
    let res = app.run(&mut terminal);
    ratatui::restore();
    match res {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("wayland-mouse tune: {e}");
            1
        }
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn ui(f: &mut Frame, app: &App) {
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(2),
    ])
    .split(f.area());

    render_tabs(f, app, chunks[0]);
    match app.tab {
        Tab::Pointer => render_curve_tab(f, app, chunks[1], true),
        Tab::Wheel => render_curve_tab(f, app, chunks[1], false),
        Tab::Buttons => render_buttons(f, app, chunks[1]),
        Tab::General => render_general(f, app, chunks[1]),
    }
    render_footer(f, app, chunks[2]);
}

fn render_tabs(f: &mut Frame, app: &App, area: Rect) {
    let titles: Vec<Line> = Tab::ALL
        .iter()
        .enumerate()
        .map(|(i, t)| Line::from(format!(" {} {} ", i + 1, t.title())))
        .collect();
    let tabs = Tabs::new(titles)
        .select(app.tab.index())
        .highlight_style(
            Style::new()
                .fg(Color::Black)
                .bg(ACCENT)
                .add_modifier(Modifier::BOLD),
        )
        .block(
            Block::bordered().border_type(BorderType::Rounded).title(
                Span::from(" wayland-mouse · live tuning ")
                    .fg(ACCENT)
                    .bold(),
            ),
        );
    f.render_widget(tabs, area);
}

fn render_curve_tab(f: &mut Frame, app: &App, area: Rect, pointer: bool) {
    let cols =
        Layout::horizontal([Constraint::Percentage(44), Constraint::Percentage(56)]).split(area);
    render_knobs(f, app, cols[0]);

    let right = Layout::vertical([Constraint::Min(0), Constraint::Length(3)]).split(cols[1]);
    render_chart(f, app, right[0], pointer);
    render_readout(f, app, right[1], pointer);
}

fn render_knobs(f: &mut Frame, app: &App, area: Rect) {
    let rows = app.rows();
    let bar_w = (area.width as usize).saturating_sub(34).clamp(6, 28);
    let mut lines = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        lines.push(knob_line(app, row, i == app.sel, bar_w));
    }
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(Span::from(" Knobs ").fg(ACCENT).bold());
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn knob_line<'a>(app: &App, row: &Row, selected: bool, bar_w: usize) -> Line<'a> {
    let marker = if selected { "▶ " } else { "  " };
    let base = if selected {
        Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
    } else {
        Style::new()
    };
    match row {
        Row::Preset => Line::from(vec![
            Span::styled(format!("{marker}{:<18}", "Preset"), base),
            Span::styled(
                format!("‹ {} ›", app.cfg.preset),
                Style::new().fg(Color::Magenta).bold(),
            ),
        ]),
        Row::Knob(field) => {
            let f = *field;
            let label = Span::styled(format!("{marker}{:<18}", f.label()), base);
            if f.is_bool() {
                let on = field_value(&app.cfg, f) > 0.5;
                let (txt, col) = if on {
                    ("ON ", Color::Green)
                } else {
                    ("OFF", BAR_EMPTY)
                };
                Line::from(vec![label, Span::styled(txt, Style::new().fg(col).bold())])
            } else {
                let v = field_value(&app.cfg, f);
                let (lo, hi) = f.bounds();
                let frac = ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
                let fill = (frac * bar_w as f64).round() as usize;
                let bar: String = "█".repeat(fill) + &"░".repeat(bar_w.saturating_sub(fill));
                let value = format!("{:>7.*}{}", f.decimals(), v, f.unit());
                Line::from(vec![
                    label,
                    Span::styled(format!("{value:>11}  "), Style::new().fg(Color::White)),
                    Span::styled(bar, Style::new().fg(BAR_FILL)),
                ])
            }
        }
    }
}

fn render_chart(f: &mut Frame, app: &App, area: Rect, pointer: bool) {
    let s = app.cfg.resolve_unscaled();
    let (curve, marker, xmax, ymax, xlabel, ylabel) = if pointer {
        // Convert device-space telemetry into config-space for the marker.
        let k = (s.dpi / REFERENCE_DPI).max(0.0001);
        let mx = app.tel.pointer_speed / k;
        let my = app.tel.pointer_gain * k;
        let xmax = (s.ptr_mid * 2.0).max(mx * 1.15).max(2000.0);
        let ymax = s.ptr_max.max(my).max(1.0) * 1.15;
        let curve = sample(xmax, |x| pointer_gain(&s, x));
        (curve, (mx, my), xmax, ymax, "mouse speed →", "gain ×")
    } else {
        let mx = app.tel.wheel_dps;
        let my = app.tel.wheel_mult;
        let xmax = (s.threshold_dps * 2.0 + 20.0).max(mx * 1.15).max(30.0);
        let ymax = s.max_mult.max(my).max(1.5) * 1.08;
        let curve = sample(xmax, |x| wheel_mult(&s, x));
        (curve, (mx, my), xmax, ymax, "scroll speed →", "× mult")
    };
    let marker_pt = [(marker.0.clamp(0.0, xmax), marker.1.clamp(0.0, ymax))];

    let datasets = vec![
        Dataset::default()
            .marker(Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::new().fg(CURVE))
            .data(&curve),
        Dataset::default()
            .marker(Marker::Braille)
            .graph_type(GraphType::Scatter)
            .style(Style::new().fg(MARKER).add_modifier(Modifier::BOLD))
            .data(&marker_pt),
    ];

    let chart = Chart::new(datasets)
        .block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .title(Span::from(" curve ").fg(ACCENT).bold()),
        )
        .x_axis(
            Axis::default()
                .title(xlabel)
                .style(Style::new().fg(Color::Gray))
                .bounds([0.0, xmax])
                .labels(vec![Span::raw("0"), Span::raw(format!("{xmax:.0}"))]),
        )
        .y_axis(
            Axis::default()
                .title(ylabel)
                .style(Style::new().fg(Color::Gray))
                .bounds([0.0, ymax])
                .labels(vec![Span::raw("0"), Span::raw(format!("{ymax:.1}"))]),
        );
    f.render_widget(chart, area);
}

fn sample(xmax: f64, g: impl Fn(f64) -> f64) -> Vec<(f64, f64)> {
    const N: usize = 80;
    (0..=N)
        .map(|i| {
            let x = xmax * i as f64 / N as f64;
            (x, g(x))
        })
        .collect()
}

fn render_readout(f: &mut Frame, app: &App, area: Rect, pointer: bool) {
    let line = if pointer {
        Line::from(vec![
            Span::raw("live  "),
            Span::styled("speed ", Style::new().fg(Color::Gray)),
            Span::styled(
                format!("{:>6.0}", app.tel.pointer_speed),
                Style::new().fg(MARKER).bold(),
            ),
            Span::styled("  gain ", Style::new().fg(Color::Gray)),
            Span::styled(
                format!("{:>4.2}×", app.tel.pointer_gain),
                Style::new().fg(MARKER).bold(),
            ),
        ])
    } else {
        Line::from(vec![
            Span::raw("live  "),
            Span::styled("scroll ", Style::new().fg(Color::Gray)),
            Span::styled(
                format!("{:>5.1}", app.tel.wheel_dps),
                Style::new().fg(MARKER).bold(),
            ),
            Span::styled(" det/s  mult ", Style::new().fg(Color::Gray)),
            Span::styled(
                format!("{:>4.2}×", app.tel.wheel_mult),
                Style::new().fg(MARKER).bold(),
            ),
        ])
    };
    let block = Block::bordered().border_type(BorderType::Rounded);
    f.render_widget(Paragraph::new(line).block(block), area);
}

fn render_buttons(f: &mut Frame, app: &App, area: Rect) {
    let mut lines = Vec::new();
    if app.cfg.button.is_empty() {
        lines.push(Line::from("No button mappings yet.".bold()));
        lines.push(Line::raw(""));
        lines.push(Line::from(
            "Find your button names:  sudo wayland-mouse buttons",
        ));
        lines.push(Line::from(
            "then add [[button]] rules to the config (see the example file).",
        ));
    } else {
        for (i, b) in app.cfg.button.iter().enumerate() {
            let sel = i == app.sel;
            let marker = if sel { "▶ " } else { "  " };
            let mode = b.mode.as_deref().unwrap_or("tap");
            let style = if sel {
                Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
            } else {
                Style::new()
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{marker}{:<12}", b.match_), style),
                Span::styled("→ ", Style::new().fg(Color::Gray)),
                Span::styled(b.keys.join(" + "), Style::new().fg(Color::Magenta)),
                Span::styled(format!("   ({mode})"), Style::new().fg(Color::Gray)),
            ]));
        }
        lines.push(Line::raw(""));
        lines.push(Line::from(
            "press d to delete · button changes apply after: sudo systemctl restart wayland-mouse"
                .fg(Color::Gray),
        ));
    }
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(Span::from(" Button mappings ").fg(ACCENT).bold());
    f.render_widget(
        Paragraph::new(lines).block(block).wrap(Wrap { trim: true }),
        area,
    );
}

fn render_general(f: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::vertical([Constraint::Length(5), Constraint::Min(0)]).split(area);

    let bar_w = (rows[0].width as usize).saturating_sub(34).clamp(6, 28);
    let knob_rows = app.rows();
    let mut lines = Vec::new();
    for (i, row) in knob_rows.iter().enumerate() {
        lines.push(knob_line(app, row, i == app.sel, bar_w));
    }
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(Span::from(" General ").fg(ACCENT).bold());
    f.render_widget(Paragraph::new(lines).block(block), rows[0]);

    let help = vec![
        Line::from("Presets set a starting point; tune any knob to override it.".fg(Color::Gray)),
        Line::raw(""),
        Line::from(vec![
            Span::styled("DPI ", Style::new().fg(ACCENT).bold()),
            Span::raw("only matters for keeping presets consistent across mice. "),
        ]),
        Line::from(
            "If you're tuning by feel on the Pointer/Wheel tabs, you can ignore it."
                .fg(Color::Gray),
        ),
        Line::raw(""),
        Line::from(
            "Press s to save your tuning to /etc/wayland-mouse/config.toml.".fg(Color::Gray),
        ),
    ];
    f.render_widget(
        Paragraph::new(help)
            .block(Block::bordered().border_type(BorderType::Rounded))
            .wrap(Wrap { trim: true }),
        rows[1],
    );
}

fn render_footer(f: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(area);

    let key = |k: &'static str| Span::styled(k, Style::new().fg(KEY).bold());
    let keys = Line::from(vec![
        key("Tab"),
        Span::raw(" tabs  "),
        key("↑↓"),
        Span::raw(" move  "),
        key("←→"),
        Span::raw(" adjust  "),
        key("⎵"),
        Span::raw(" toggle  "),
        key("p"),
        Span::raw(" preset  "),
        key("s"),
        Span::raw(" save  "),
        key("r"),
        Span::raw(" reset  "),
        key("q"),
        Span::raw(" quit"),
    ]);
    f.render_widget(Paragraph::new(keys), rows[0]);

    let dirty = if app.dirty {
        Span::styled("●unsaved", Style::new().fg(MARKER).bold())
    } else {
        Span::styled("✓saved", Style::new().fg(Color::Green))
    };
    let status = Line::from(vec![
        Span::styled(
            format!("preset {}  ", app.cfg.preset),
            Style::new().fg(Color::Magenta),
        ),
        dirty,
        Span::raw("   "),
        Span::styled(app.status.clone(), Style::new().fg(Color::Gray)),
    ]);
    f.render_widget(Paragraph::new(status).alignment(Alignment::Left), rows[1]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_value_reads_preset_defaults() {
        let cfg = ConfigFile::default();
        assert!((field_value(&cfg, Field::MaxGain) - 2.5).abs() < 1e-9);
        assert!((field_value(&cfg, Field::StartSpeed) - 8.0).abs() < 1e-9);
        assert!(field_value(&cfg, Field::PtrEnabled) > 0.5);
    }

    #[test]
    fn adjust_writes_override_and_snaps() {
        let mut cfg = ConfigFile::default();
        field_adjust(&mut cfg, Field::MaxGain, 1.0); // 2.5 -> 2.6
        assert_eq!(cfg.pointer.max_gain, Some(2.6));
        field_adjust(&mut cfg, Field::MaxGain, -1.0); // back to 2.5
        assert!((cfg.pointer.max_gain.unwrap() - 2.5).abs() < 1e-9);
    }

    #[test]
    fn adjust_clamps_to_bounds() {
        let mut cfg = ConfigFile::default();
        for _ in 0..1000 {
            field_adjust(&mut cfg, Field::MaxGain, 1.0);
        }
        assert_eq!(cfg.pointer.max_gain, Some(8.0)); // upper bound
    }

    #[test]
    fn toggle_bool_field() {
        let mut cfg = ConfigFile::default();
        assert!(field_value(&cfg, Field::WheelEnabled) > 0.5);
        field_adjust(&mut cfg, Field::WheelEnabled, 1.0);
        assert_eq!(cfg.wheel.enabled, Some(false));
    }

    #[test]
    fn preset_cycles_and_wraps() {
        let mut cfg = ConfigFile::default(); // mac-like
        cycle_preset(&mut cfg, 1);
        assert_eq!(cfg.preset, "subtle");
        cycle_preset(&mut cfg, 1);
        assert_eq!(cfg.preset, "off");
        cycle_preset(&mut cfg, 1);
        assert_eq!(cfg.preset, "mac-like"); // wrapped
        cycle_preset(&mut cfg, -1);
        assert_eq!(cfg.preset, "off");
    }

    #[test]
    fn curves_are_monotonic_ish() {
        let s = ConfigFile::default().resolve_unscaled();
        assert!(pointer_gain(&s, 0.0) < pointer_gain(&s, 10000.0));
        assert!(wheel_mult(&s, 0.0) <= wheel_mult(&s, 50.0));
    }

    // A Client wired to a socketpair whose peer is dropped — enough to build an
    // App for render tests (`ui` never touches the client).
    fn dummy_client() -> Client {
        let (a, _b) = UnixStream::pair().unwrap();
        let reader = BufReader::new(a.try_clone().unwrap());
        Client { writer: a, reader }
    }

    #[test]
    fn renders_every_tab_without_panicking() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = App::new(dummy_client(), ConfigFile::default());
        app.cfg.button.push(crate::config::ButtonRule {
            match_: "BTN_SIDE".into(),
            keys: vec!["Super".into(), "Page_Up".into()],
            mode: None,
        });
        app.tel = TelemetrySample {
            pointer_speed: 1500.0,
            pointer_gain: 1.8,
            wheel_dps: 10.0,
            wheel_mult: 2.5,
        };

        let mut term = Terminal::new(TestBackend::new(140, 36)).unwrap();
        for _ in 0..Tab::ALL.len() {
            term.draw(|f| ui(f, &app)).unwrap();
            app.switch_tab(1);
        }
        // Also render at a tiny size to catch layout underflow.
        let mut tiny = Terminal::new(TestBackend::new(20, 8)).unwrap();
        tiny.draw(|f| ui(f, &app)).unwrap();
    }
}
