//! The integrations behind the Flow view's nodes: the background
//! `guard status --integrations --json` poll, and the side panel that shows
//! one integration and, where the GUI can, configures it.
//!
//! Secrets: the Jira token is read from the Credential Manager on the worker
//! thread right before the child starts, reaches it through its environment
//! only ([`integrations::child_env`]) and is redacted from its output.

use super::GuardApp;
use super::widgets;
use crate::accounts::{self, TestabilityEdit};
use crate::commands::GuardCommand;
use crate::integrations::{self, Form, Integration, State};
use crate::runner;
use crate::theme;
use eframe::egui::{self, Color32, CornerRadius, FontId, Margin, RichText, Stroke, Ui};
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// `guard status --integrations`: every probe is bounded by 2 s and they
/// run in parallel; this leaves room for a cold start of the binary.
const INTEGRATIONS_TIMEOUT: Duration = Duration::from_secs(20);

/// Whether the Credential Manager holds a Jira token for the settings' site.
pub(super) fn jira_token_saved(settings: &integrations::IntegrationSettings) -> bool {
    settings.jira_host().is_some_and(
        |host| matches!(crate::credentials::load_jira(&host), Ok(Some(t)) if !t.trim().is_empty()),
    )
}

/// "Animate controls and elements inside windows" is off in Windows'
/// accessibility settings, or `GUARD_GUI_REDUCED_MOTION=1`.
pub(super) fn os_reduced_motion() -> bool {
    if std::env::var("GUARD_GUI_REDUCED_MOTION").is_ok_and(|v| v.trim() == "1") {
        return true;
    }
    windows_reduced_motion()
}

#[cfg(windows)]
fn windows_reduced_motion() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW,
    };
    let mut on: i32 = 1;
    // SAFETY: SPI_GETCLIENTAREAANIMATION writes one BOOL into `on`, which
    // lives for the whole call.
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            (&mut on as *mut i32).cast(),
            0,
        )
    };
    ok != 0 && on == 0
}

#[cfg(not(windows))]
fn windows_reduced_motion() -> bool {
    false
}

/// The colour of a state's badge.
pub(super) fn state_color(state: State) -> Color32 {
    match state {
        State::Connected => theme::VERIFIED,
        State::Configured | State::Unknown => theme::FOCUS,
        State::NotConfigured => theme::NEUTRAL,
        State::Disconnected => theme::AT_RISK,
        State::Error => theme::BROKEN,
        State::Working => theme::ACCENT,
    }
}

impl GuardApp {
    /// The report, when one is in.
    pub(super) fn integration_report(&self) -> Option<&integrations::Report> {
        self.integrations.as_ref().and_then(|r| r.as_ref().ok())
    }

    /// The panel fields that come from `target.toml`.
    pub(super) fn load_config_inputs(&mut self) {
        self.jira_project_input.clear();
        self.server_url_input.clear();
        self.testability_input = TestabilityEdit::default();
        let Some(repo) = self.repo() else {
            return;
        };
        let Ok(text) = std::fs::read_to_string(repo.join("target.toml")) else {
            return;
        };
        if let Ok(config) = guard_core::config::parse_target(&text) {
            self.jira_project_input = config.tracker.and_then(|t| t.project).unwrap_or_default();
            self.server_url_input = config.server.map(|s| s.url).unwrap_or_default();
        }
        if let Some(edit) = TestabilityEdit::from_target(&text) {
            self.testability_input = edit;
        }
    }

    /// Starts the report off the UI thread (one at a time).
    pub(super) fn start_integrations(&mut self, ctx: &egui::Context) {
        if self.integrations_job.is_some() || self.shot.is_some() {
            return;
        }
        // Every attempt counts, failed ones too: the next is 30 s away, not
        // the next frame.
        self.integrations_at = Some(Instant::now());
        let Some(repo) = self.repo().map(Path::to_path_buf) else {
            return;
        };
        let Ok(guard) = self.guard.clone() else {
            self.integrations = Some(Err(
                "guard binary not found: set GUARD_BIN or add it to PATH.".into(),
            ));
            return;
        };
        let settings = self.integration_settings.clone();
        // The token is saved per host: only an https site gets it.
        let use_token = self.jira_token_saved && integrations::jira_token_allowed(&settings);
        let gui_jira = integrations::jira_override(&settings).is_some();
        let (tx, rx) = mpsc::channel();
        let repaint = ctx.clone();
        let command = GuardCommand::Integrations;
        std::thread::spawn(move || {
            let token = settings
                .jira_host()
                .filter(|_| use_token)
                .and_then(|host| crate::credentials::load_jira(&host).ok().flatten());
            let env = integrations::child_env(&settings, token.as_deref());
            let out = runner::capture_with_env(
                &guard,
                &command.args(),
                &env,
                &repo,
                INTEGRATIONS_TIMEOUT,
            )
            .map(|mut c| {
                c.stdout = crate::ai_settings::redact(&c.stdout, token.as_deref());
                c.stderr = crate::ai_settings::redact(&c.stderr, token.as_deref());
                c
            })
            .map_err(|e| format!("{}: {e}", command.display()));
            drop(env);
            let _ = tx.send(out);
            repaint.request_repaint();
        });
        self.integrations_job = Some(super::IntegrationsJob {
            epoch: self.epoch,
            rx,
            gui_jira,
        });
    }

    /// Asks again now (the ↻ button, a saved setting): a call in flight is
    /// dropped, since it ran with the old settings.
    pub(super) fn refresh_integrations(&mut self, ctx: &egui::Context) {
        self.integrations_job = None;
        self.start_integrations(ctx);
    }

    /// On project open, then every 30 s while the Flow view is visible.
    pub(super) fn poll_integrations(&mut self, ctx: &egui::Context) {
        if self.project.is_none() || self.shot.is_some() {
            return;
        }
        let visible = self.view == super::View::Flow;
        let now = Instant::now();
        if self.integrations_job.is_none()
            && integrations::poll_due(self.integrations_at, now, visible)
        {
            self.start_integrations(ctx);
        }
        if visible {
            let wait = integrations::next_poll_in(self.integrations_at, now);
            ctx.request_repaint_after(wait.max(Duration::from_secs(1)));
        }
    }

    pub(super) fn drain_integrations(&mut self) {
        let Some(job) = &self.integrations_job else {
            return;
        };
        let Ok(out) = job.rx.try_recv() else {
            return;
        };
        let (epoch, gui_jira) = (job.epoch, job.gui_jira);
        self.integrations_job = None;
        if epoch != self.epoch {
            return;
        }
        self.integrations = Some(out.and_then(|c| {
            if c.code == Some(0) && !c.stdout.trim().is_empty() {
                let mut report = integrations::parse(&c.stdout)?;
                if gui_jira {
                    integrations::mark_gui_jira_probe(&mut report);
                }
                Ok(report)
            } else {
                Err(super::error_lines(&c))
            }
        }));
    }

    /// `edit` applied to the project's target.toml, written only when the
    /// result is still a valid target.toml.
    fn edit_target(&self, edit: impl Fn(&str) -> Result<String, String>) -> Result<(), String> {
        let path = self
            .repo()
            .map(|r| r.join("target.toml"))
            .ok_or("no project is open")?;
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("no se puede leer target.toml: {}", e.kind()))?;
        let new = edit(&text)?;
        if new != text {
            std::fs::write(&path, new)
                .map_err(|e| format!("no se puede escribir target.toml: {}", e.kind()))?;
        }
        Ok(())
    }

    fn save_jira(&mut self, ctx: &egui::Context) {
        let result = (|| {
            let s = &mut self.integration_settings;
            s.apply_jira(&self.jira_base_url_input, &self.jira_email_input)?;
            self.jira_base_url_input = s.jira_base_url.clone();
            self.jira_email_input = s.jira_email.clone();
            let token = self.jira_token_input.trim().to_string();
            if !token.is_empty() {
                let host = s
                    .jira_host()
                    .ok_or("escribe la URL del sitio antes de guardar el token")?;
                if !integrations::jira_token_allowed(s) {
                    return Err("el token solo se envía a un sitio https".to_string());
                }
                crate::credentials::save_jira(&host, &token)?;
            }
            let key = self.jira_project_input.trim().to_string();
            if !key.is_empty() {
                self.edit_target(|text| integrations::set_tracker_project(text, &key))?;
            }
            Ok::<_, String>(())
        })();
        self.jira_token_input.clear();
        self.jira_token_saved = jira_token_saved(&self.integration_settings);
        self.after_save(ctx, result, "Jira guardado. Probando la conexión…");
    }

    fn forget_jira_token(&mut self) {
        if let Some(host) = self.integration_settings.jira_host() {
            self.config_message = Some(match crate::credentials::clear_jira(&host) {
                Ok(()) => (true, format!("Token de {host} eliminado.")),
                Err(e) => (false, e),
            });
        }
        self.jira_token_saved = jira_token_saved(&self.integration_settings);
    }

    fn save_server(&mut self, ctx: &egui::Context) {
        let url = self.server_url_input.trim().to_string();
        let result = if url.is_empty() {
            Err("escribe la URL de guard serve".to_string())
        } else {
            self.edit_target(|text| integrations::set_server_url(text, &url))
        };
        self.after_save(ctx, result, "[server] guardado en target.toml. Probando…");
    }

    fn save_gateway(&mut self, ctx: &egui::Context) {
        let result = self
            .integration_settings
            .apply_gateway(&self.gateway_url_input);
        if result.is_ok() {
            self.gateway_url_input = self.integration_settings.gateway_url.clone();
        }
        self.after_save(ctx, result, "Gateway guardado. Probando…");
    }

    fn add_role(&mut self) {
        match accounts::add_role(&self.testability_input.roles, &self.new_role_input) {
            Ok(roles) => {
                self.testability_input.roles = roles;
                self.new_role_input.clear();
                self.config_message = None;
            }
            Err(e) => self.config_message = Some((false, e)),
        }
    }

    fn save_accounts(&mut self, ctx: &egui::Context) {
        let edit = TestabilityEdit {
            login_path: self.testability_input.login_path.trim().to_string(),
            token_env: self.testability_input.token_env.trim().to_string(),
            roles: self.testability_input.roles.clone(),
        };
        let result = self.edit_target(|text| accounts::set_testability(text, &edit));
        if result.is_ok() {
            self.testability_input = edit;
        }
        self.after_save(ctx, result, "[testability] guardado en target.toml.");
    }

    fn after_save(&mut self, ctx: &egui::Context, result: Result<(), String>, ok: &str) {
        match result {
            Ok(()) => {
                self.config_message = Some((true, ok.to_string()));
                self.refresh_integrations(ctx);
            }
            Err(e) => self.config_message = Some((false, e)),
        }
    }

    /// The integration section of the inspector for `node`.
    pub(super) fn integration_panel(&mut self, ui: &mut Ui, node: &str) {
        let integration = self
            .integration_report()
            .and_then(|r| r.for_node(node))
            .cloned();
        let form = integrations::form(node);
        if integration.is_none() && form.is_none() {
            return;
        }
        egui::Frame::new()
            .fill(Color32::from_rgb(0x1f, 0x20, 0x23))
            .stroke(Stroke::new(1.0, theme::BORDER))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin::same(12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 6.0;
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("INTEGRACIÓN")
                            .font(FontId::monospace(10.5))
                            .color(theme::TEXT_SECONDARY),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let busy = self.integrations_job.is_some();
                        let tip = if busy {
                            "Consultando…"
                        } else {
                            "Volver a consultar (guard status --integrations)"
                        };
                        if widgets::icon_button(ui, widgets::Icon::Refresh, theme::TEXT, !busy, tip)
                            .clicked()
                        {
                            self.refresh_integrations(ui.ctx());
                        }
                    });
                });
                match &integration {
                    Some(i) => {
                        let connect = i.gui_probe.then(|| self.guard_connect_jira());
                        status_block(ui, i, connect.as_deref());
                    }
                    None => {
                        let text = match &self.integrations {
                            Some(Err(e)) => format!("No se pudo consultar guard: {e}"),
                            _ => "Consultando el estado…".to_string(),
                        };
                        ui.label(RichText::new(text).size(12.5).color(theme::TEXT_SECONDARY));
                    }
                }
                if let Some(form) = form {
                    ui.add_space(4.0);
                    ui.separator();
                    match form {
                        Form::Jira => self.jira_form(ui),
                        Form::Server => self.server_form(ui),
                        Form::Gateway => self.gateway_form(ui),
                        Form::Accounts => self.accounts_form(ui),
                    }
                }
                if let Some((ok, message)) = &self.config_message {
                    let color = if *ok { theme::VERIFIED } else { theme::BROKEN };
                    ui.label(RichText::new(message).size(12.0).color(color));
                }
            });
        ui.add_space(8.0);
    }

    /// The `guard connect jira` that gives guard itself the panel's site,
    /// email and key; the token goes in on stdin, never in argv.
    fn guard_connect_jira(&self) -> String {
        let s = &self.integration_settings;
        let key = self.jira_project_input.trim();
        format!(
            "guard connect jira --base-url {} --email {} --project {} --token-stdin",
            s.jira_base_url,
            s.jira_email,
            if key.is_empty() { "<KEY>" } else { key },
        )
    }

    fn jira_form(&mut self, ui: &mut Ui) {
        hint(
            ui,
            "Si ya conectaste guard con guard connect jira, deja el sitio vacío: guard usa esas credenciales.",
        );
        field(
            ui,
            "Sitio de Jira",
            &mut self.jira_base_url_input,
            "https://acme.atlassian.net",
        );
        field(
            ui,
            "Email de la cuenta",
            &mut self.jira_email_input,
            "qa@acme.com",
        );
        field(
            ui,
            "Clave del proyecto",
            &mut self.jira_project_input,
            "SHOP",
        );
        // The saved site: the one a token is stored for.
        let host = self.integration_settings.jira_host();
        let token_hint = match (&host, self.jira_token_saved) {
            (Some(host), true) => format!("guardado para {host}"),
            (Some(host), false) => format!("se guarda en Credenciales de Windows ({host})"),
            (None, _) => "primero el sitio".to_string(),
        };
        ui.label(
            RichText::new("Token de API")
                .size(12.0)
                .color(theme::TEXT_SECONDARY),
        );
        ui.add(
            egui::TextEdit::singleline(&mut self.jira_token_input)
                .password(true)
                .hint_text(token_hint)
                .desired_width(f32::INFINITY),
        );
        ui.horizontal(|ui| {
            if widgets::run_button(ui, "Guardar y probar", true).clicked() {
                self.save_jira(ui.ctx());
            }
            if self.jira_token_saved && widgets::flat_button(ui, "Olvidar token", true).clicked() {
                self.forget_jira_token();
            }
        });
        hint(
            ui,
            "El sitio, el email y el token solo los usa esta app (JIRA_BASE_URL, JIRA_EMAIL y JIRA_API_TOKEN de guard status). guard serve, finalize y los agentes leen Jira de su entorno o de guard connect jira. La clave va a [tracker] de target.toml.",
        );
    }

    fn server_form(&mut self, ui: &mut Ui) {
        hint(
            ui,
            "El triaje y el gatekeeper corren dentro de guard serve. Su URL va a [server] de target.toml.",
        );
        field(
            ui,
            "URL de guard serve",
            &mut self.server_url_input,
            "https://guard.acme.com",
        );
        if widgets::run_button(ui, "Guardar y probar", true).clicked() {
            self.save_server(ui.ctx());
        }
    }

    fn gateway_form(&mut self, ui: &mut Ui) {
        hint(
            ui,
            "Sobrescribe [gateway] url de agents.toml para esta app (GUARD_GATEWAY_ADMIN_URL).",
        );
        field(
            ui,
            "URL de LiteLLM",
            &mut self.gateway_url_input,
            "http://localhost:4000",
        );
        if widgets::run_button(ui, "Guardar y probar", true).clicked() {
            self.save_gateway(ui.ctx());
        }
    }
}

impl GuardApp {
    fn accounts_form(&mut self, ui: &mut Ui) {
        ui.label(
            RichText::new("Cuentas y roles")
                .size(12.5)
                .color(theme::TEXT),
        );
        hint(
            ui,
            "Cada rol inicia sesión con E2E_<ROL>_EMAIL y E2E_<ROL>_PASSWORD (el rol en mayúsculas, con - como _). Defínelas en el entorno: los secretos nunca se escriben en target.toml y esta app solo comprueba si existen.",
        );
        let mut remove = None;
        for role in &self.testability_input.roles {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(role)
                        .font(FontId::monospace(12.0))
                        .color(theme::TEXT),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::flat_button(ui, "Quitar", true).clicked() {
                        remove = Some(role.clone());
                    }
                });
            });
            let status = accounts::credential_status(role, accounts::env_present);
            let (email, password) = accounts::credential_env_names(role);
            presence(ui, &email, status.email);
            presence(ui, &password, status.password);
        }
        if let Some(role) = remove {
            let roles = &self.testability_input.roles;
            self.testability_input.roles = accounts::remove_role(roles, &role);
        }
        if self.testability_input.roles.is_empty() {
            hint(ui, "Sin roles: guard no prepara sesiones de login.");
        }
        self.new_role_row(ui);
        field(
            ui,
            "Ruta de login (login_path)",
            &mut self.testability_input.login_path,
            "/__test__/login",
        );
        field(
            ui,
            "Variable del token (token_env)",
            &mut self.testability_input.token_env,
            "GUARD_TEST_TOKEN",
        );
        let token_env = self.testability_input.token_env.trim();
        if accounts::check_token_env(token_env).is_ok() {
            presence(ui, token_env, accounts::env_present(token_env));
        }
        if widgets::run_button(ui, "Guardar en target.toml", true).clicked() {
            self.save_accounts(ui.ctx());
        }
    }

    /// The new role's field and "Añadir rol" (Enter adds too).
    fn new_role_row(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            let input = ui.add(
                egui::TextEdit::singleline(&mut self.new_role_input)
                    .hint_text("nuevo rol, p. ej. admin")
                    .desired_width((ui.available_width() - 100.0).max(80.0)),
            );
            let enter = input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if widgets::flat_button(ui, "Añadir rol", true).clicked() || enter {
                self.add_role();
            }
        });
    }
}

/// `✔ NAME` / `✘ NAME`: whether the variable is set, never its value.
fn presence(ui: &mut Ui, name: &str, set: bool) {
    let (mark, color, state) = if set {
        ('\u{2714}', theme::VERIFIED, "definida")
    } else {
        ('\u{2718}', theme::BROKEN, "sin definir")
    };
    ui.label(
        RichText::new(format!("  {mark} {name} · {state}"))
            .font(FontId::monospace(11.5))
            .color(color),
    );
}

fn hint(ui: &mut Ui, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).size(11.5).color(theme::TEXT_DISABLED)).wrap());
}

fn field(ui: &mut Ui, label: &str, value: &mut String, placeholder: &str) {
    ui.label(RichText::new(label).size(12.0).color(theme::TEXT_SECONDARY));
    ui.add(
        egui::TextEdit::singleline(value)
            .hint_text(placeholder)
            .desired_width(f32::INFINITY),
    );
}

/// The state badge, a Spanish summary over guard's own detail, the running
/// tasks and the setup (copyable only when it is one guard command).
/// `connect`: the command that hands guard the panel's Jira settings.
fn status_block(ui: &mut Ui, i: &Integration, connect: Option<&str>) {
    ui.horizontal(|ui| {
        widgets::chip(ui, &i.state.label().to_uppercase(), state_color(i.state));
        ui.label(
            RichText::new(integrations::spanish_label(i))
                .size(12.5)
                .color(theme::TEXT),
        );
    });
    ui.add(
        egui::Label::new(
            RichText::new(integrations::summary(i))
                .size(12.5)
                .color(theme::TEXT),
        )
        .wrap(),
    );
    ui.add(
        egui::Label::new(
            RichText::new(&i.detail)
                .font(FontId::monospace(11.5))
                .color(theme::TEXT_SECONDARY),
        )
        .wrap(),
    );
    for task in &i.tasks {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(egui::Vec2::splat(12.0), egui::Sense::hover());
            widgets::dot(ui.painter(), r.center(), 3.5, state_color(State::Working));
            ui.label(
                RichText::new(format!(
                    "{} · {} · {} (intento {})",
                    task.ticket,
                    if task.role == "qa" {
                        "Agente QA"
                    } else {
                        "Agente Coder"
                    },
                    task.branch,
                    task.attempts
                ))
                .size(12.5)
                .color(theme::TEXT),
            );
        });
    }
    if let Some(cmd) = connect {
        setup(ui, "Para que guard use estas credenciales", cmd, true);
    } else if let Some(cmd) = &i.configure {
        setup(
            ui,
            "Para configurarlo",
            cmd,
            integrations::copy_command(cmd).is_some(),
        );
    }
}

/// guard's setup hint in monospace, with "Copiar comando" when it is one.
fn setup(ui: &mut Ui, title: &str, text: &str, command: bool) {
    ui.label(RichText::new(title).size(12.0).color(theme::TEXT_SECONDARY));
    egui::Frame::new()
        .fill(Color32::from_rgb(0x19, 0x1a, 0x1d))
        .stroke(Stroke::new(1.0, theme::BORDER))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(
                egui::Label::new(
                    RichText::new(text)
                        .font(FontId::monospace(11.5))
                        .color(theme::TEXT),
                )
                .wrap(),
            );
        });
    if command && widgets::flat_button(ui, "Copiar comando", true).clicked() {
        ui.ctx().copy_text(text.trim().to_string());
    }
}
