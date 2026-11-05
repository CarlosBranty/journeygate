//! The E2E flow diagram, ported from `docs/flujo-e2e.html`: its nodes, edges,
//! lanes, walkthrough steps and modes (the texts stay in Spanish: they are
//! content), the grid layout and wire geometry in canvas units, the
//! step-by-step walk, and the live mapping from a guard run to the nodes of
//! the local test path. Everything here is pure; `app::flow_view` paints it.

use eframe::egui::{Pos2, Rect, Vec2, pos2, vec2};
use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
mod tests;

/// The colour role of a node or wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Ok,
    Warn,
    Err,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Node {
    pub id: &'static str,
    /// The kicker: stage number and name.
    pub stage: &'static str,
    pub title: &'static str,
    pub sub: &'static str,
    pub body: &'static str,
    pub bullets: &'static [&'static str],
    /// Commands, shown as a code block.
    pub cmds: &'static [&'static str],
    pub chips: &'static [&'static str],
    /// Sections of the architecture document.
    pub refs: &'static str,
    /// Infrastructure and context: a dashed, transparent card.
    pub sat: bool,
    pub tone: Tone,
}

const fn n(
    id: &'static str,
    stage: &'static str,
    title: &'static str,
    sub: &'static str,
    body: &'static str,
    bullets: &'static [&'static str],
    refs: &'static str,
) -> Node {
    Node {
        id,
        stage,
        title,
        sub,
        body,
        bullets,
        cmds: &[],
        chips: &[],
        refs,
        sat: false,
        tone: Tone::Plain,
    }
}

pub const NODES: [Node; 18] = [
    n(
        "jira",
        "01 · Origen",
        "Jira",
        "Ticket nuevo o cambio de estado",
        "Todo requerimiento nace en Jira. Un webhook firmado llega al orquestador con la descripción y los criterios de aceptación del ticket.",
        &[
            "Entrada: POST /webhooks/jira",
            "El triaje solo se aplica a tickets: decide quién desarrolla",
            "Los tickets que cambian un flujo existente reciben la etiqueta flow-change",
        ],
        "Secciones 1 y 14",
    ),
    n(
        "ingest",
        "01 · Ingesta",
        "Orquestador Rust",
        "Axum · análisis de impacto",
        "Rust identifica los módulos que menciona el ticket y calcula con tree-sitter cuántos otros módulos dependen de ellos.",
        &[
            "Un plugin de tree-sitter por lenguaje; sin plugin, usa los globs de target.toml",
            "El fan-out llega a Jev como bucket (aislado, moderado o amplio), nunca como número",
            "Las rutas sensibles de target.toml quedan marcadas como regla dura",
            "Con cada PR participa por un costado: le da al guard el contexto del ticket y recibe el resultado para Jira",
        ],
        "Secciones 1, 2 y 7",
    ),
    n(
        "jev",
        "02 · Triaje",
        "Jev AI · jev-1.13.0",
        "7 preguntas en una sola pasada",
        "Un único POST /v1/systemone evalúa todas las preguntas sobre el mismo state.",
        &[
            "Choice: área del sistema y tipo de requerimiento",
            "Score: riesgo de 0 a 4 (los niveles empiezan en 0)",
            "Noul: criterios claros, pagos o seguridad, apto para agente y cambio de un flujo existente",
            "Si Jev no responde, el ticket va al carril humano",
        ],
        "Sección 2",
    ),
    n(
        "gate",
        "02 · Triaje",
        "Gatekeeper determinista",
        "Reglas duras primero",
        "Rust decide el carril. Las reglas sobre hechos van primero y Jev no puede anularlas; después se aplican los umbrales calibrados.",
        &[
            "Carril humano si toca rutas sensibles, migraciones o un fan-out amplio",
            "Carril agente solo con apto > 0.80, riesgo < 1.0, pagos < 0.15, confianza > 0.75 y cambio de flujo < 0.20",
            "En Shadow Mode decide, registra y deriva todo al humano",
        ],
        "Sección 2",
    ),
    n(
        "qa",
        "03A · Carril agente",
        "Agente QA",
        "Escribe el spec del ticket",
        "Un contenedor nuevo escribe tests/features/KEY.spec.ts con Playwright MCP contra el entorno preview. Es la única ruta en la que puede escribir.",
        &[
            "Sigue AGENTS.md: Page Objects, fixtures, tags y datos por seed",
            "Tag obligatorio @ticket:KEY",
        ],
        "Secciones 4 y 13",
    ),
    n(
        "seal",
        "03A · Carril agente",
        "Red-first y sellado",
        "El test debe fallar antes del cambio",
        "El orquestador corre el spec contra el commit base: si pasa, no prueba nada nuevo. Después lo sella con un hash guardado fuera del sandbox.",
        &[
            "Chequeos AST: un expect real por test, sin skip, only ni waitForTimeout",
            "La señal de Jev sobre la calidad del test es secundaria",
        ],
        "Sección 4",
    ),
    n(
        "coder",
        "03A · Carril agente",
        "Agente Coder",
        "Implementa hasta que el spec pase",
        "Otro contenedor recibe el spec sellado en solo lectura y solo puede escribir código de la app. No ve los journeys de regresión.",
        &[
            "Runtimes: Claude Code, Codex, Copilot CLI o mini-swe-agent",
            "Todo el tráfico a modelos pasa por el gateway",
            "Máximo 3 intentos por ticket",
        ],
        "Secciones 4 y 16",
    ),
    n(
        "dev",
        "03B · Carril humano",
        "Dev",
        "Ticket con informe de impacto",
        "El ticket llega asignado con los archivos sugeridos y los journeys que probablemente haya que actualizar.",
        &["Llega por reglas duras, por umbrales o porque el ticket cambia un flujo existente"],
        "Secciones 2 y 14",
    ),
    Node {
        cmds: &[
            "guard up",
            "guard test",
            "guard test --headed --journey checkout.customer-card",
            "guard test --all --docker",
        ],
        ..n(
            "local",
            "03B · Carril humano",
            "Prueba local",
            "El mismo motor que el guard",
            "El dev corre los journeys afectados antes del PR, con o sin un asistente de IA conectado al servidor MCP.",
            &[],
            "Secciones 8 y 17",
        )
    },
    n(
        "devpr",
        "03B · Carril humano",
        "PR del dev",
        "Con el journey actualizado si cambia un flujo",
        "Si el cambio de flujo es intencional, el journey se actualiza en el mismo PR y lo aprueba QA vía CODEOWNERS.",
        &["El guard marca menos aserciones, un skip nuevo o un journey modificado sin flow-change"],
        "Sección 14",
    ),
    n(
        "direct",
        "04 · Entrada directa",
        "PR o pedido de QA",
        "Hotfix, Dependabot o release candidate",
        "Un PR ya está desarrollado: entra directo al guard desde el pipeline, sin pasar por el triaje. Si trae clave de Jira, el orquestador aporta el contexto del ticket y recibe el resultado.",
        &[
            "Lo dispara cada PR en GitHub o GitLab, no el webhook de Jira",
            "Un PR sin ticket también pasa la suite completa: una dependencia actualizada rompe cosas",
            "Un pedido de QA antes de producción corre el mismo pipeline de forma manual",
        ],
        "Sección 12",
    ),
    Node {
        sat: true,
        ..n(
            "skills",
            "Infraestructura",
            "Skills y AGENTS.md",
            "Una fuente para todos los asistentes",
            "guard agents sync renderiza las mismas convenciones para cada runtime y para el asistente del dev. Las reglas duras viven en el sandbox, no en el prompt.",
            &[],
            "Sección 16",
        )
    },
    Node {
        sat: true,
        ..n(
            "gateway",
            "Infraestructura",
            "Gateway de modelos",
            "LiteLLM · llaves virtuales",
            "Cada tarea recibe una llave con presupuesto, modelos permitidos y vencimiento. Las llaves reales de los proveedores nunca entran al sandbox.",
            &[],
            "Sección 16",
        )
    },
    Node {
        chips: &["plan", "shards por duración", "finalize"],
        ..n(
            "guard",
            "04 · Guard remoto",
            "Guard remoto en CI",
            "Suite completa · GitHub Actions o GitLab CI",
            "Corre todos los journeys activos más el smoke. Cada shard levanta su propia app y su base de datos.",
            &[
                "Primera pasada liviana: solo screenshot al fallar",
                "Los shards se reparten por duración histórica",
                "Jev ya no decide qué se ejecuta",
            ],
            "Secciones 9 y 12",
        )
    },
    Node {
        chips: &["reintentos", "baseline"],
        ..n(
            "confirm",
            "04 · Determinismo",
            "Confirmación del fallo",
            "3 intentos y corrida contra la base",
            "Un fallo solo es regresión si falla los 3 intentos y pasa en el commit base.",
            &[
                "Video y trace solo en el primer reintento de un test que falló",
                "Imagen de Docker, reloj, zona horaria y semilla fijos",
            ],
            "Sección 10",
        )
    },
    Node {
        tone: Tone::Warn,
        ..n(
            "flaky",
            "04 · Determinismo",
            "Inestable",
            "Pasa en un reintento",
            "Queda en cuarentena y no bloquea, salvo que sea smoke o un journey afectado por el diff.",
            &[
                "Ticket automático al dueño del journey",
                "Aparece en la vista Inestables del tablero",
            ],
            "Secciones 6 y 10",
        )
    },
    Node {
        tone: Tone::Ok,
        ..n(
            "green",
            "05 · Salida",
            "Verde",
            "Merge queue · staging o producción",
            "El check obligatorio pasa, la merge queue revalida contra el main actual y el cambio sigue a staging o producción.",
            &["Resumen en el PR o MR y en Jira"],
            "Secciones 5 y 12",
        )
    },
    Node {
        tone: Tone::Err,
        ..n(
            "red",
            "05 · Salida",
            "Regresión",
            "Bloqueo y evidencia en Jira",
            "El merge se bloquea. Se crea o actualiza un Bug en Jira con el screenshot adjunto y links al trace y al video.",
            &[
                "Vuelve al Agente Coder como entrada del siguiente intento, o al dev o autor del PR con la línea exacta",
                "guard repro reproduce el fallo en local con la misma imagen y semilla",
            ],
            "Sección 11",
        )
    },
];

pub fn node(id: &str) -> Option<&'static Node> {
    NODES.iter().find(|n| n.id == id)
}

/// The grid column of a node: 1 to 3, or the whole row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Col {
    At(u8),
    Span,
}

/// `(node, grid row, column)`, as `POS` in the HTML.
pub const POS: [(&str, u8, Col); 18] = [
    ("jira", 1, Col::At(2)),
    ("ingest", 2, Col::At(2)),
    ("jev", 3, Col::At(2)),
    ("gate", 4, Col::At(2)),
    ("qa", 5, Col::At(1)),
    ("skills", 5, Col::At(2)),
    ("dev", 5, Col::At(3)),
    ("seal", 6, Col::At(1)),
    ("gateway", 6, Col::At(2)),
    ("local", 6, Col::At(3)),
    ("coder", 7, Col::At(1)),
    ("direct", 7, Col::At(2)),
    ("devpr", 7, Col::At(3)),
    ("guard", 8, Col::Span),
    ("confirm", 9, Col::At(2)),
    ("flaky", 9, Col::At(3)),
    ("green", 10, Col::At(1)),
    ("red", 10, Col::At(3)),
];

const ROWS: usize = 10;

/// How a wire is routed between two cards.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Route {
    /// Bottom centre to top centre, shifted by `to_dx` at the target.
    V { to_dx: f32 },
    /// Right side to left side.
    Right,
    /// Left side to right side.
    Left,
    /// The feedback loop under the outcome row, up the left of the target.
    FbLeft,
    /// The feedback loop up the right of both cards.
    FbRight,
    /// The side channel along the canvas' left edge.
    Ctx,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Edge {
    pub id: &'static str,
    pub from: &'static str,
    pub to: &'static str,
    pub route: Route,
    pub tone: Tone,
    pub feedback: bool,
    pub sat: bool,
    /// Two-way side channel (the orchestrator next to the guard).
    pub side: bool,
}

const fn e(id: &'static str, from: &'static str, to: &'static str, route: Route) -> Edge {
    Edge {
        id,
        from,
        to,
        route,
        tone: Tone::Plain,
        feedback: false,
        sat: false,
        side: false,
    }
}

const V: Route = Route::V { to_dx: 0.0 };

pub const EDGES: [Edge; 22] = [
    e("jira-ingest", "jira", "ingest", V),
    e("ingest-jev", "ingest", "jev", V),
    e("jev-gate", "jev", "gate", V),
    e("gate-qa", "gate", "qa", V),
    e("gate-dev", "gate", "dev", V),
    e("qa-seal", "qa", "seal", V),
    e("seal-coder", "seal", "coder", V),
    e("dev-local", "dev", "local", V),
    e("local-devpr", "local", "devpr", V),
    e("coder-guard", "coder", "guard", Route::V { to_dx: -170.0 }),
    e("direct-guard", "direct", "guard", V),
    e("devpr-guard", "devpr", "guard", Route::V { to_dx: 170.0 }),
    e("guard-confirm", "guard", "confirm", V),
    Edge {
        tone: Tone::Warn,
        ..e("confirm-flaky", "confirm", "flaky", Route::Right)
    },
    e("confirm-green", "confirm", "green", V),
    Edge {
        tone: Tone::Err,
        ..e("confirm-red", "confirm", "red", V)
    },
    Edge {
        tone: Tone::Err,
        feedback: true,
        ..e("red-coder", "red", "coder", Route::FbLeft)
    },
    Edge {
        tone: Tone::Err,
        feedback: true,
        ..e("red-dev", "red", "dev", Route::FbRight)
    },
    Edge {
        sat: true,
        ..e("skills-qa", "skills", "qa", Route::Left)
    },
    Edge {
        sat: true,
        ..e("skills-dev", "skills", "dev", Route::Right)
    },
    Edge {
        sat: true,
        ..e("gateway-coder", "gateway", "coder", Route::Left)
    },
    Edge {
        sat: true,
        side: true,
        ..e("guard-ingest", "guard", "ingest", Route::Ctx)
    },
];

/// The label along the side channel.
pub const SIDE_LABEL: &str = "contexto del ticket · resultado a Jira";

/// The legend, in the HTML's order.
pub const LEGEND: [(&str, LegendMark); 4] = [
    ("Flujo activo", LegendMark::Flow),
    ("Regresión", LegendMark::Err),
    ("Inestable", LegendMark::Warn),
    ("Infraestructura y contexto", LegendMark::Sat),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegendMark {
    Flow,
    Err,
    Warn,
    Sat,
}

pub const FOOT: &str = "Basado en la Propuesta de Arquitectura de la Plataforma E2E Agéntica. Cada paso indica la sección del documento que lo detalla.";
pub const HINT: &str = "Haz clic en un paso para ver qué ocurre ahí, o usa Play o Step para recorrer el flujo paso a paso.";

/// Which journey through the diagram is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Full,
    Agent,
    Human,
    Direct,
}

impl Mode {
    pub const ALL: [Mode; 4] = [Mode::Full, Mode::Agent, Mode::Human, Mode::Direct];

    /// The segmented-control label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Full => "Completo",
            Self::Agent => "Carril agente",
            Self::Human => "Carril humano",
            Self::Direct => "Entrada directa",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Full => "Flujo completo",
            Self::Agent => "Carril agente",
            Self::Human => "Carril humano",
            Self::Direct => "Entrada directa",
        }
    }

    pub fn text(self) -> &'static str {
        match self {
            Self::Full => {
                "Los requerimientos nacen en Jira y pasan por el triaje. Todo PR, venga de un carril o entre directo, converge en el guard remoto, que corre la suite completa antes de cualquier merge."
            }
            Self::Agent => {
                "Para tickets simples y de bajo riesgo: el Agente QA escribe el test, se sella, y el Agente Coder implementa sin poder tocarlo."
            }
            Self::Human => {
                "Arquitectura, pagos, cambios de flujo o zona gris: un dev toma el ticket ya clasificado y prueba en local con el mismo motor que el guard."
            }
            Self::Direct => {
                "Un PR ya desarrollado, o un pedido de QA antes de producción, entra directo al guard remoto, sin triaje. El orquestador participa por un costado: aporta el contexto del ticket y recibe el resultado para Jira."
            }
        }
    }

    /// The nodes lit while idle (`LANE`).
    pub fn lane(self) -> &'static [&'static str] {
        match self {
            Self::Full => &[
                "jira", "ingest", "jev", "gate", "qa", "seal", "coder", "dev", "local", "devpr",
                "direct", "guard", "confirm", "skills", "gateway",
            ],
            Self::Agent => &[
                "jira", "ingest", "jev", "gate", "qa", "seal", "coder", "guard", "confirm",
                "skills", "gateway",
            ],
            Self::Human => &[
                "jira", "ingest", "jev", "gate", "dev", "local", "devpr", "guard", "confirm",
                "skills",
            ],
            Self::Direct => &["direct", "guard", "confirm", "ingest"],
        }
    }

    /// The walkthrough before its outcome (`STEPS`).
    pub fn steps(self) -> &'static [&'static str] {
        match self {
            Self::Full => &[
                "jira", "ingest", "jev", "gate", "qa", "seal", "coder", "dev", "local", "devpr",
                "direct", "guard", "confirm",
            ],
            Self::Agent => &[
                "jira", "ingest", "jev", "gate", "qa", "seal", "coder", "guard", "confirm",
            ],
            Self::Human => &[
                "jira", "ingest", "jev", "gate", "dev", "local", "devpr", "guard", "confirm",
            ],
            Self::Direct => &["direct", "guard", "confirm"],
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Agent => "agent",
            Self::Human => "human",
            Self::Direct => "direct",
        }
    }
}

/// The two simulation switches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Sim {
    pub reg: bool,
    pub flaky: bool,
}

pub const SIM_REG: &str = "Simulación: un journey falla los 3 intentos y pasa en la base. El merge se bloquea y la evidencia vuelve al carril de origen.";
pub const SIM_FLAKY: &str =
    "Simulación: un test falla y pasa en un reintento. Queda en cuarentena y no bloquea.";

/// Seconds each walkthrough step stays on screen.
pub const STEP_SECS: f32 = 2.3;
/// Seconds the comet takes along the wire into a step.
pub const COMET_SECS: f32 = 0.9;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step {
    pub id: &'static str,
    /// The feedback edge this step is entered through.
    pub via: Option<&'static str>,
}

pub fn build_steps(mode: Mode, sim: Sim) -> Vec<Step> {
    let mut steps: Vec<Step> = mode
        .steps()
        .iter()
        .map(|id| Step { id, via: None })
        .collect();
    let step = |id, via| Step { id, via };
    if sim.flaky {
        steps.push(step("flaky", None));
    }
    if sim.reg {
        steps.push(step("red", None));
        if matches!(mode, Mode::Full | Mode::Agent) {
            steps.push(step("coder", Some("red-coder")));
        }
        if matches!(mode, Mode::Full | Mode::Human) {
            steps.push(step("dev", Some("red-dev")));
        }
    } else {
        steps.push(step("green", None));
    }
    steps
}

pub fn edge(id: &str) -> Option<&'static Edge> {
    EDGES.iter().find(|e| e.id == id)
}

pub fn edge_allowed(e: &Edge, sim: Sim) -> bool {
    if e.to == "green" {
        return !sim.reg;
    }
    if e.to == "red" || e.feedback {
        return sim.reg;
    }
    if e.to == "flaky" {
        return sim.flaky;
    }
    true
}

/// The step-by-step walkthrough ("Recorrer paso a paso").
#[derive(Debug, Clone, PartialEq)]
pub struct Walk {
    pub mode: Mode,
    steps: Vec<Step>,
    index: usize,
    visited: BTreeSet<&'static str>,
    feedback_shown: BTreeSet<&'static str>,
    comet: Option<(&'static str, Tone)>,
    /// Seconds since the current step was entered.
    elapsed: f32,
}

impl Walk {
    /// Starts on the first step.
    pub fn start(mode: Mode, sim: Sim) -> Self {
        let mut walk = Self {
            mode,
            steps: build_steps(mode, sim),
            index: 0,
            visited: BTreeSet::new(),
            feedback_shown: BTreeSet::new(),
            comet: None,
            elapsed: 0.0,
        };
        walk.enter(sim);
        walk
    }

    fn enter(&mut self, sim: Sim) {
        self.elapsed = 0.0;
        let Some(step) = self.steps.get(self.index).copied() else {
            self.comet = None;
            return;
        };
        let via = match step.via {
            Some(via) => {
                self.feedback_shown.insert(via);
                Some(via)
            }
            None => {
                let prev = self.index.checked_sub(1).map(|i| self.steps[i].id);
                let candidates: Vec<&Edge> = EDGES
                    .iter()
                    .filter(|e| {
                        e.to == step.id
                            && !e.feedback
                            && !e.sat
                            && self.visited.contains(e.from)
                            && edge_allowed(e, sim)
                    })
                    .collect();
                candidates
                    .iter()
                    .find(|e| Some(e.from) == prev)
                    .or(candidates.first())
                    .map(|e| e.id)
            }
        };
        self.visited.insert(step.id);
        self.comet = via.and_then(edge).map(|e| (e.id, e.tone));
    }

    /// Moves to the next step; false once past the last one.
    pub fn advance(&mut self, sim: Sim) -> bool {
        if self.finished() {
            return false;
        }
        self.index += 1;
        self.enter(sim);
        !self.finished()
    }

    /// Lets `dt` seconds pass; true when that entered a new step (or ended).
    pub fn tick(&mut self, dt: f32, sim: Sim) -> bool {
        if self.finished() {
            return false;
        }
        self.elapsed += dt;
        if self.elapsed >= STEP_SECS {
            self.advance(sim);
            return true;
        }
        false
    }

    pub fn finished(&self) -> bool {
        self.index >= self.steps.len()
    }

    /// The node of the current step (empty once finished).
    pub fn current(&self) -> &'static str {
        self.steps.get(self.index).map_or("", |s| s.id)
    }

    /// `(1-based step, step count)`.
    pub fn position(&self) -> (usize, usize) {
        ((self.index + 1).min(self.steps.len()), self.steps.len())
    }

    pub fn len(&self) -> usize {
        self.steps.len()
    }

    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// The edge the current step was entered through, and its tone.
    pub fn comet(&self) -> Option<(&'static str, Tone)> {
        self.comet
    }

    /// 0 to 1 along the comet's edge, eased.
    pub fn comet_progress(&self) -> f32 {
        ease((self.elapsed / COMET_SECS).min(1.0))
    }
}

/// Ease-in-out quad, as the HTML's comet.
pub fn ease(k: f32) -> f32 {
    if k < 0.5 {
        2.0 * k * k
    } else {
        1.0 - (-2.0 * k + 2.0).powi(2) / 2.0
    }
}

/// The lit nodes: the walk's visited ones (plus their infrastructure), or
/// the mode's lane plus its outcome.
pub fn active_nodes(mode: Mode, sim: Sim, walk: Option<&Walk>) -> BTreeSet<&'static str> {
    if let Some(walk) = walk {
        let mut set = walk.visited.clone();
        if set.contains("qa") || set.contains("dev") {
            set.insert("skills");
        }
        if set.contains("coder") && walk.mode != Mode::Human {
            set.insert("gateway");
        }
        if set.contains("guard") {
            set.insert("ingest");
        }
        return set;
    }
    let mut set: BTreeSet<&'static str> = mode.lane().iter().copied().collect();
    if sim.flaky {
        set.insert("flaky");
    }
    set.insert(if sim.reg { "red" } else { "green" });
    set
}

pub fn active_edges(
    nodes: &BTreeSet<&'static str>,
    sim: Sim,
    walk: Option<&Walk>,
) -> Vec<&'static Edge> {
    EDGES
        .iter()
        .filter(|e| {
            if !nodes.contains(e.from) || !nodes.contains(e.to) || !edge_allowed(e, sim) {
                return false;
            }
            match walk {
                Some(walk) if e.feedback => walk.feedback_shown.contains(e.id),
                _ => true,
            }
        })
        .collect()
}

/// Everything up- and downstream of `id` along the main wires.
pub fn lineage(id: &'static str, edges: &[&Edge]) -> BTreeSet<&'static str> {
    let mut keep = BTreeSet::from([id]);
    for down in [true, false] {
        let mut stack = vec![id];
        while let Some(cur) = stack.pop() {
            for e in edges.iter().filter(|e| !e.feedback && !e.side) {
                let next = if down {
                    (e.from == cur).then_some(e.to)
                } else {
                    (e.to == cur).then_some(e.from)
                };
                if let Some(next) = next
                    && keep.insert(next)
                {
                    stack.push(next);
                }
            }
        }
    }
    keep
}

// Layout, in canvas units (CSS pixels of the HTML).
pub const COL_W: f32 = 250.0;
pub const WIDE_W: f32 = 620.0;
pub const SAT_W: f32 = 220.0;
pub const COL_GAP: f32 = 48.0;
pub const ROW_GAP: f32 = 50.0;
pub const PAD_T: f32 = 28.0;
pub const PAD_X: f32 = 44.0;
pub const PAD_B: f32 = 76.0;
/// The x of the side channel.
pub const CTX_X: f32 = 10.0;
const LOOP_OUT: f32 = 26.0;
const CORNER: f32 = 14.0;

pub fn node_width(node: &Node) -> f32 {
    if node.id == "guard" {
        WIDE_W
    } else if node.sat {
        SAT_W
    } else {
        COL_W
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub rects: BTreeMap<&'static str, Rect>,
    pub size: Vec2,
}

/// The widest the cards grow to use spare horizontal space.
pub const MAX_SPREAD: f32 = 2.2;
/// Below this zoom the text floors shrink with the zoom.
pub const FLOOR_ZOOM: f32 = 0.5;

/// On-screen text size for `base` canvas pixels at zoom `z`: never
/// below `floor` while the zoom is at least [`FLOOR_ZOOM`], so a fitted
/// diagram stays readable; further out the floor scales down with the
/// zoom, which bounds a card at twice its base size in canvas units.
pub fn text_px(base: f32, floor: f32, z: f32) -> f32 {
    (base * z).max(floor * (z / FLOOR_ZOOM).min(1.0))
}

/// How much wider than [`COL_W`] the columns get for a view `view_w`
/// canvas units wide: they fill it, between 1 and [`MAX_SPREAD`].
pub fn spread(view_w: f32) -> f32 {
    ((view_w - 2.0 * PAD_X - 2.0 * COL_GAP) / (3.0 * COL_W)).clamp(1.0, MAX_SPREAD)
}

/// Places every card on the grid; `height` measures a card at its width.
/// Rows are as tall as their tallest card, and cards centre in their cell.
pub fn layout(height: impl Fn(&Node) -> f32) -> Layout {
    layout_spread(1.0, |n, _| height(n))
}

/// [`layout`] with every card `s` times wider; gaps and padding keep
/// their size. `height` gets the card and its width.
pub fn layout_spread(s: f32, height: impl Fn(&Node, f32) -> f32) -> Layout {
    let width = |n: &Node| node_width(n) * s;
    let col_w = COL_W * s;
    let heights: BTreeMap<&str, f32> = NODES.iter().map(|n| (n.id, height(n, width(n)))).collect();
    let mut row_h = [0.0f32; ROWS];
    for (id, row, _) in POS {
        let h = &mut row_h[usize::from(row) - 1];
        *h = h.max(heights[id]);
    }
    let grid_w = 3.0 * col_w + 2.0 * COL_GAP;
    let mut row_top = [0.0f32; ROWS];
    let mut y = PAD_T;
    for (top, h) in row_top.iter_mut().zip(row_h) {
        *top = y;
        y += h + ROW_GAP;
    }
    let size = vec2(2.0 * PAD_X + grid_w, y - ROW_GAP + PAD_B);
    let rects = POS
        .iter()
        .map(|&(id, row, col)| {
            let node = node(id).expect("POS names a node");
            let r = usize::from(row) - 1;
            let cx = match col {
                Col::At(c) => PAD_X + f32::from(c - 1) * (col_w + COL_GAP) + col_w / 2.0,
                Col::Span => PAD_X + grid_w / 2.0,
            };
            let h = heights[id];
            let cy = row_top[r] + row_h[r] / 2.0;
            let rect = Rect::from_center_size(pos2(cx, cy), vec2(width(node), h));
            (id, rect)
        })
        .collect();
    Layout { rects, size }
}

fn cubic(p0: Pos2, p1: Pos2, p2: Pos2, p3: Pos2, out: &mut Vec<Pos2>) {
    const N: usize = 24;
    for i in 1..=N {
        let t = i as f32 / N as f32;
        let u = 1.0 - t;
        let w = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
        out.push(pos2(
            w[0] * p0.x + w[1] * p1.x + w[2] * p2.x + w[3] * p3.x,
            w[0] * p0.y + w[1] * p1.y + w[2] * p2.y + w[3] * p3.y,
        ));
    }
}

/// A polyline with its inner corners rounded by quadratic arcs.
fn rounded(pts: &[Pos2], radius: f32) -> Vec<Pos2> {
    let mut out = vec![pts[0]];
    for w in pts.windows(3) {
        let (p, c, nx) = (w[0], w[1], w[2]);
        let d1 = p.distance(c);
        let d2 = c.distance(nx);
        let k = radius.min(d1 / 2.0).min(d2 / 2.0);
        let a = c - (c - p) / d1 * k;
        let b = c + (nx - c) / d2 * k;
        out.push(a);
        const N: usize = 8;
        for i in 1..=N {
            let t = i as f32 / N as f32;
            let u = 1.0 - t;
            out.push(pos2(
                u * u * a.x + 2.0 * u * t * c.x + t * t * b.x,
                u * u * a.y + 2.0 * u * t * c.y + t * t * b.y,
            ));
        }
    }
    out.push(*pts.last().expect("at least two points"));
    out
}

fn curve_x(a: Pos2, b: Pos2) -> Vec<Pos2> {
    let mx = (a.x + b.x) / 2.0;
    let mut out = vec![a];
    cubic(a, pos2(mx, a.y), pos2(mx, b.y), b, &mut out);
    out
}

/// The wire of `e` as a polyline, in canvas units, from source to target.
pub fn edge_points(e: &Edge, rects: &BTreeMap<&'static str, Rect>) -> Vec<Pos2> {
    let a = rects[e.from];
    let b = rects[e.to];
    match e.route {
        Route::V { to_dx } => {
            let my = (a.bottom() + b.top()) / 2.0;
            let start = pos2(a.center().x, a.bottom());
            let bx = b.center().x + to_dx;
            let mut out = vec![start];
            cubic(
                start,
                pos2(start.x, my),
                pos2(bx, my),
                pos2(bx, b.top()),
                &mut out,
            );
            out
        }
        Route::Right => curve_x(pos2(a.right(), a.center().y), pos2(b.left(), b.center().y)),
        Route::Left => curve_x(pos2(a.left(), a.center().y), pos2(b.right(), b.center().y)),
        Route::FbLeft => {
            let floor = a.bottom().max(rects["green"].bottom()) + LOOP_OUT;
            let x = b.left() - LOOP_OUT;
            rounded(
                &[
                    pos2(a.center().x, a.bottom()),
                    pos2(a.center().x, floor),
                    pos2(x, floor),
                    pos2(x, b.center().y),
                    pos2(b.left(), b.center().y),
                ],
                CORNER,
            )
        }
        Route::FbRight => {
            let x = a.right().max(b.right()) + LOOP_OUT;
            rounded(
                &[
                    pos2(a.right(), a.center().y),
                    pos2(x, a.center().y),
                    pos2(x, b.center().y),
                    pos2(b.right(), b.center().y),
                ],
                CORNER,
            )
        }
        Route::Ctx => rounded(
            &[
                pos2(a.left(), a.center().y),
                pos2(CTX_X, a.center().y),
                pos2(CTX_X, b.center().y),
                pos2(b.left(), b.center().y),
            ],
            CORNER,
        ),
    }
}

/// The point at fraction `t` of the polyline's length.
pub fn point_at(pts: &[Pos2], t: f32) -> Pos2 {
    let total: f32 = pts.windows(2).map(|w| w[0].distance(w[1])).sum();
    if total <= 0.0 {
        return pts.first().copied().unwrap_or(Pos2::ZERO);
    }
    let mut left = t.clamp(0.0, 1.0) * total;
    for w in pts.windows(2) {
        let d = w[0].distance(w[1]);
        if left <= d && d > 0.0 {
            return w[0] + (w[1] - w[0]) * (left / d);
        }
        left -= d;
    }
    *pts.last().expect("not empty")
}

// Live mode: the local test path while guard-gui runs guard.

/// How a finished test run ended, in the diagram's words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Verde.
    Green,
    /// Regresión.
    Regression,
    /// Inestable: passed, with tests that passed on a retry.
    Flaky,
    /// Cancelled, killed, or failed without a failed test.
    Stopped,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Self::Green => "Verde",
            Self::Regression => "Regresión",
            Self::Flaky => "Inestable",
            Self::Stopped => "Detenida",
        }
    }
}

/// The verdict of a finished `guard test` from its exit and its report.
pub fn verdict(success: bool, cancelled: bool, failed: usize, flaky: usize) -> Verdict {
    match (cancelled, success) {
        (true, _) => Verdict::Stopped,
        (false, true) if flaky > 0 => Verdict::Flaky,
        (false, true) => Verdict::Green,
        (false, false) if failed > 0 => Verdict::Regression,
        (false, false) => Verdict::Stopped,
    }
}

/// What kind of guard command is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunKind {
    /// `guard up`.
    Up,
    /// `guard test ...`.
    Test,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LivePhase {
    Up,
    /// The affected preview that picks the journeys to run.
    Selecting,
    Testing,
    Done(Verdict),
}

impl LivePhase {
    /// The inspector title of the phase.
    pub fn title(self) -> &'static str {
        match self {
            Self::Up => "Levantando servicios",
            Self::Selecting => "Seleccionando journeys afectados",
            Self::Testing => "Corrida Playwright",
            Self::Done(Verdict::Green) => "Verde",
            Self::Done(Verdict::Regression) => "Regresión",
            Self::Done(Verdict::Flaky) => "Inestable",
            Self::Done(Verdict::Stopped) => "Corrida detenida",
        }
    }

    /// What happens in the phase, for the inspector.
    pub fn text(self) -> &'static str {
        match self {
            Self::Up => "guard up levanta la aplicación y sus servicios para la corrida local.",
            Self::Selecting => {
                "guard affected lee el diff para elegir los journeys que el cambio puede romper."
            }
            Self::Testing => {
                "Playwright ejecuta los journeys seleccionados. Un fallo se reintenta antes de contar."
            }
            Self::Done(Verdict::Green) => "Todos los journeys pasaron: listo para el PR.",
            Self::Done(Verdict::Regression) => {
                "Al menos un journey falló en todos sus intentos. Los fallos están en Problems."
            }
            Self::Done(Verdict::Flaky) => {
                "La corrida pasó, pero algunos journeys solo pasaron en un reintento."
            }
            Self::Done(Verdict::Stopped) => {
                "La última corrida se canceló, guard no pudo ejecutar las pruebas o no escribió un reporte nuevo."
            }
        }
    }
}

/// The live phase from what runs now, else the last test run's verdict.
pub fn live_phase(
    running: Option<RunKind>,
    selecting: bool,
    last: Option<Verdict>,
) -> Option<LivePhase> {
    match running {
        Some(RunKind::Up) => return Some(LivePhase::Up),
        Some(RunKind::Test) => return Some(LivePhase::Testing),
        _ => {}
    }
    if selecting {
        return Some(LivePhase::Selecting);
    }
    if running.is_some() {
        return None;
    }
    last.map(LivePhase::Done)
}

/// The four stages of the live strip.
pub const LIVE_STEPS: [&str; 4] = [
    "guard up",
    "Journeys afectados",
    "Corrida Playwright",
    "Resultado",
];

/// The nodes a live phase lights, the one it is on, and its stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Live {
    pub lit: Vec<&'static str>,
    pub current: Option<&'static str>,
    pub tone: Tone,
    /// Index into [`LIVE_STEPS`].
    pub step: usize,
}

pub fn live(phase: LivePhase) -> Live {
    let local = vec!["dev", "local"];
    let tested = vec!["dev", "local", "confirm"];
    let with = |mut base: Vec<&'static str>, more: &[&'static str]| {
        base.extend_from_slice(more);
        base
    };
    let (lit, current, tone, step) = match phase {
        LivePhase::Up => (local, Some("local"), Tone::Plain, 0),
        LivePhase::Selecting => (local, Some("local"), Tone::Plain, 1),
        LivePhase::Testing => (tested, Some("confirm"), Tone::Plain, 2),
        LivePhase::Done(Verdict::Green) => (with(tested, &["green"]), Some("green"), Tone::Ok, 3),
        LivePhase::Done(Verdict::Regression) => (with(tested, &["red"]), Some("red"), Tone::Err, 3),
        LivePhase::Done(Verdict::Flaky) => (
            with(tested, &["flaky", "green"]),
            Some("flaky"),
            Tone::Warn,
            3,
        ),
        LivePhase::Done(Verdict::Stopped) => (local, None, Tone::Plain, 3),
    };
    Live {
        lit,
        current,
        tone,
        step,
    }
}

/// The Flow view state the screenshot harness starts from:
/// `mode=<full|agent|human|direct>,select=<node>,step=<n>,sim=<reg|flaky|both>,live=<up|select|test|green|red|flaky|stopped>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ShotSpec {
    pub mode: Mode,
    pub select: Option<&'static str>,
    /// Walk paused on this 1-based step.
    pub step: Option<usize>,
    pub sim: Sim,
    pub live: Option<LivePhase>,
}

impl ShotSpec {
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut spec = Self::default();
        for part in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let bad = || format!("invalid flow setting: {part}");
            let (key, value) = part.split_once('=').ok_or_else(bad)?;
            match key {
                "mode" => {
                    spec.mode = Mode::ALL
                        .into_iter()
                        .find(|m| m.key() == value)
                        .ok_or_else(bad)?
                }
                "select" => spec.select = Some(node(value).ok_or_else(bad)?.id),
                "step" => spec.step = Some(value.parse().ok().filter(|n| *n > 0).ok_or_else(bad)?),
                "sim" => {
                    spec.sim = match value {
                        "reg" => Sim {
                            reg: true,
                            flaky: false,
                        },
                        "flaky" => Sim {
                            reg: false,
                            flaky: true,
                        },
                        "both" => Sim {
                            reg: true,
                            flaky: true,
                        },
                        _ => return Err(bad()),
                    }
                }
                "live" => {
                    spec.live = Some(match value {
                        "up" => LivePhase::Up,
                        "select" => LivePhase::Selecting,
                        "test" => LivePhase::Testing,
                        "green" => LivePhase::Done(Verdict::Green),
                        "red" => LivePhase::Done(Verdict::Regression),
                        "flaky" => LivePhase::Done(Verdict::Flaky),
                        "stopped" => LivePhase::Done(Verdict::Stopped),
                        _ => return Err(bad()),
                    })
                }
                _ => return Err(bad()),
            }
        }
        Ok(spec)
    }
}

/// The camera pan that keeps `content` (screen size, origin at the pan) on a
/// `view` of that size: content larger than the view never leaves an empty
/// band (its top/left edge never below the view's, its bottom/right edge never
/// above the view's); content that fits is centred.
pub fn clamp_pan(target: Vec2, content: Vec2, view: Vec2) -> Vec2 {
    let axis = |t: f32, c: f32, v: f32| {
        if c <= v {
            (v - c) / 2.0
        } else {
            t.clamp(v - c, 0.0)
        }
    };
    vec2(
        axis(target.x, content.x, view.x),
        axis(target.y, content.y, view.y),
    )
}
