//! Reapers for what a crashed or killed attempt leaves behind (sandbox.md,
//! tokens.md): gateway virtual keys that were never revoked, and guard
//! sandbox networks that no container uses any more.
//!
//! Both are idempotent passes meant to run on a timer in `guard serve` (next
//! to `DockerSandbox::reap_expired`, which removes containers whose deadline
//! passed). Neither touches anything younger than its `min_age`, so a key or
//! network of an attempt that is still starting is never reaped.
//!
//! - [`KeyReaper`]: lists keys by alias prefix through
//!   [`ModelGateway::list_keys`], keeps only the ones the agent lane minted
//!   ([`lane_run_id`]: the gateway may be shared), and revokes every one whose run is not
//!   active ([`RunActivity`]; [`PgRunActivity`] reads `task_queue` and
//!   `agent_runs`), and every key that already expired.
//! - [`NetworkReaper`]: removes `guard.sandbox`-labelled networks with no
//!   container attached ([`DockerNetworks`]; [`CliDockerNetworks`] runs the
//!   `docker` CLI).

use guard_core::ports::StoreError;
use guard_core::ports::model_gateway::{GatewayError, KeyInfo, ModelGateway};
use guard_store::map_sqlx_error;
use guard_store::sqlx::{self, PgPool};
use std::sync::Arc;
use std::time::Duration;

/// Default `min_age` of both reapers.
pub const DEFAULT_MIN_AGE: Duration = Duration::from_secs(15 * 60);
/// Alias prefix of the agent lane's keys (`a<unix>-<ticket>:<role>:<attempt>`,
/// guard-cli `new_run_id`).
pub const LANE_ALIAS_PREFIX: &str = "a";
/// Label every sandbox container and network carries (guard-sandbox
/// `LABEL_SANDBOX`; value: the sandbox name).
pub const SANDBOX_LABEL: &str = "guard.sandbox";
/// Default sandbox name prefix (guard-sandbox `DockerConfig::name_prefix`).
pub const SANDBOX_NAME_PREFIX: &str = "guard-sbx";

// ---------------------------------------------------------------- time

/// Days since 1970-01-01 of a civil date (H. Hinnant's `days_from_civil`).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (i64::from(m) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Unix seconds of an RFC 3339 timestamp (`2026-11-01T10:00:00Z`, with
/// optional fraction and `±hh:mm` offset; a space for `T` and a missing
/// offset, read as UTC, are accepted too). `None` when unparsable.
pub fn parse_rfc3339(raw: &str) -> Option<i64> {
    let s = raw.trim();
    let (date, rest) = s.split_at_checked(10)?;
    let rest = rest.strip_prefix(['T', 't', ' '])?;
    let mut d = date.split('-');
    let (y, mo, da): (i64, u32, u32) = (
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
    );
    let (time, offset) = match rest.find(['Z', 'z', '+', '-']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let time = time.split('.').next()?.trim();
    let mut t = time.split(':');
    let (h, mi, se): (i64, i64, i64) = (
        t.next()?.parse().ok()?,
        t.next()?.parse().ok()?,
        t.next().unwrap_or("0").parse().ok()?,
    );
    if !(1..=12).contains(&mo) || !(1..=31).contains(&da) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    let offset_secs = match offset {
        "" | "Z" | "z" => 0,
        o => {
            // `+02:00`, or Go's default `+0000 UTC`.
            let sign = if o.starts_with('-') { -1 } else { 1 };
            let digits: String = o[1..]
                .split_whitespace()
                .next()?
                .chars()
                .filter(|c| *c != ':')
                .collect();
            if digits.len() != 4 {
                return None;
            }
            let (oh, om) = digits.split_at(2);
            sign * (oh.parse::<i64>().ok()? * 3600 + om.parse::<i64>().ok()? * 60)
        }
    };
    Some(days_from_civil(y, mo, da) * 86_400 + h * 3600 + mi * 60 + se - offset_secs)
}

fn old_enough(created: Option<&str>, now: i64, min_age: Duration) -> bool {
    // An unknown creation time counts as old: the run check still applies.
    created
        .and_then(parse_rfc3339)
        .is_none_or(|c| now.saturating_sub(c) >= min_age.as_secs() as i64)
}

// ---------------------------------------------------------------- keys

/// What the orchestrator knows about a run id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    /// A worker holds a live lease on a task of the run.
    Active,
    /// The run has `agent_runs` rows and no live task.
    Finished,
    /// Neither: a crashed worker before any row, or not a lane run.
    Unknown,
}

#[async_trait::async_trait]
pub trait RunActivity: Send + Sync {
    async fn state(&self, run_id: &str) -> Result<RunState, StoreError>;
}

/// [`RunActivity`] over `task_queue` (running, lease not expired, payload
/// `input.run_id`) and `agent_runs`.
pub struct PgRunActivity {
    pool: PgPool,
}

impl PgRunActivity {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl RunActivity for PgRunActivity {
    async fn state(&self, run_id: &str) -> Result<RunState, StoreError> {
        let (active, recorded): (bool, bool) = sqlx::query_as(
            "SELECT EXISTS (SELECT 1 FROM task_queue
                            WHERE state = 'running' AND locked_until > now()
                              AND payload -> 'input' ->> 'run_id' = $1),
                    EXISTS (SELECT 1 FROM agent_runs WHERE run_id = $1)",
        )
        .bind(run_id)
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(match (active, recorded) {
            (true, _) => RunState::Active,
            (false, true) => RunState::Finished,
            (false, false) => RunState::Unknown,
        })
    }
}

/// Whether `run` has the shape of a lane run id (`a<unix>-<ticket>`,
/// guard-cli `new_run_id`: lowercase ASCII alphanumerics and `-`).
pub fn is_lane_run_id(run: &str) -> bool {
    let Some(rest) = run.strip_prefix('a') else {
        return false;
    };
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let Some(ticket) = rest[digits..].strip_prefix('-') else {
        return false;
    };
    digits > 0
        && !ticket.is_empty()
        && ticket
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The run id of a key guard's agent lane minted, `None` for every other
/// key. A lane key's alias is `<run_id>:<role>:<attempt>[:c<claim>][:<agent>]`
/// with `role` `qa` or `coder` and a numeric attempt, its run id satisfies
/// `run_shape`, and its `run_id` metadata, when the gateway reports it,
/// names the same run.
pub fn lane_run_id(key: &KeyInfo, run_shape: fn(&str) -> bool) -> Option<String> {
    let alias = key.alias.as_deref()?;
    let mut parts = alias.split(':');
    let run = parts.next()?;
    let role = parts.next()?;
    let attempt = parts.next()?;
    let owned = run_shape(run)
        && matches!(role, "qa" | "coder")
        && !attempt.is_empty()
        && attempt.bytes().all(|b| b.is_ascii_digit())
        && key
            .metadata
            .get("run_id")
            .is_none_or(|m| m.is_empty() || m == run);
    owned.then(|| run.to_string())
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyReapReport {
    /// Lane keys ([`lane_run_id`]) the gateway listed under the prefix;
    /// keys guard did not mint are not counted and never touched.
    pub listed: usize,
    /// Revoked keys: `(alias, reason)`.
    pub revoked: Vec<(String, String)>,
    /// Keys left alone (active run or too young).
    pub kept: usize,
    /// Per-key failures (the pass goes on).
    pub errors: Vec<String>,
}

pub struct KeyReaper {
    pub gateway: Arc<dyn ModelGateway>,
    pub runs: Arc<dyn RunActivity>,
    /// Only keys whose alias starts with this ([`LANE_ALIAS_PREFIX`]).
    pub alias_prefix: String,
    /// Which run ids are guard's ([`is_lane_run_id`]); keys of any other
    /// run are never touched (the gateway may be shared).
    pub run_shape: fn(&str) -> bool,
    /// Keys younger than this are kept whatever their run's state.
    pub min_age: Duration,
}

impl KeyReaper {
    pub fn new(gateway: Arc<dyn ModelGateway>, runs: Arc<dyn RunActivity>) -> Self {
        Self {
            gateway,
            runs,
            alias_prefix: LANE_ALIAS_PREFIX.to_string(),
            run_shape: is_lane_run_id,
            min_age: DEFAULT_MIN_AGE,
        }
    }

    pub fn with_alias_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.alias_prefix = prefix.into();
        self
    }

    pub fn with_run_shape(mut self, run_shape: fn(&str) -> bool) -> Self {
        self.run_shape = run_shape;
        self
    }

    pub fn with_min_age(mut self, min_age: Duration) -> Self {
        self.min_age = min_age;
        self
    }

    /// One pass at `now` (unix seconds). Listing failures abort the pass;
    /// a failed run lookup or revocation is recorded and the pass goes on.
    pub async fn reap(&self, now: i64) -> Result<KeyReapReport, GatewayError> {
        let keys: Vec<(KeyInfo, String)> = self
            .gateway
            .list_keys(&self.alias_prefix)
            .await?
            .into_iter()
            .filter_map(|k| lane_run_id(&k, self.run_shape).map(|run| (k, run)))
            .collect();
        let mut report = KeyReapReport {
            listed: keys.len(),
            ..KeyReapReport::default()
        };
        for (key, run) in &keys {
            let alias = key.alias.clone().unwrap_or_else(|| key.key_id.clone());
            let expired = key
                .expires_at
                .as_deref()
                .and_then(parse_rfc3339)
                .is_some_and(|e| e <= now);
            let reason = if expired {
                Some("expired".to_string())
            } else if !old_enough(key.created_at.as_deref(), now, self.min_age) {
                None
            } else {
                match self.runs.state(run).await {
                    Ok(RunState::Active) => None,
                    Ok(RunState::Finished) => Some(format!("run {run} finished")),
                    Ok(RunState::Unknown) => Some(format!("run {run} unknown")),
                    Err(e) => {
                        report.errors.push(format!("{alias}: run lookup: {e}"));
                        None
                    }
                }
            };
            let Some(reason) = reason else {
                report.kept += 1;
                continue;
            };
            match self.gateway.revoke(&key.key_id).await {
                Ok(()) | Err(GatewayError::NotFound(_)) => report.revoked.push((alias, reason)),
                Err(e) => report.errors.push(format!("{alias}: revoke: {e}")),
            }
        }
        Ok(report)
    }
}

// ---------------------------------------------------------------- networks

/// One labelled network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkInfo {
    pub name: String,
    /// Value of the [`SANDBOX_LABEL`] label (the sandbox name).
    pub sandbox: String,
    /// Containers attached.
    pub containers: usize,
    /// RFC 3339.
    pub created_at: Option<String>,
}

#[async_trait::async_trait]
pub trait DockerNetworks: Send + Sync {
    /// Every network carrying the `label` label.
    async fn list(&self, label: &str) -> Result<Vec<NetworkInfo>, String>;
    async fn remove(&self, name: &str) -> Result<(), String>;
}

/// [`DockerNetworks`] over the `docker` CLI (`docker network ls` / `inspect` / `rm`).
pub struct CliDockerNetworks {
    pub program: String,
    pub timeout: Duration,
}

impl Default for CliDockerNetworks {
    fn default() -> Self {
        Self {
            program: "docker".into(),
            timeout: Duration::from_secs(60),
        }
    }
}

impl CliDockerNetworks {
    async fn docker(&self, args: &[&str]) -> Result<String, String> {
        let run = tokio::process::Command::new(&self.program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output();
        let out = tokio::time::timeout(self.timeout, run)
            .await
            .map_err(|_| format!("docker {} timed out", args.join(" ")))?
            .map_err(|e| format!("cannot run {}: {e}", self.program))?;
        if !out.status.success() {
            return Err(format!(
                "docker {} failed: {}",
                args.first().copied().unwrap_or_default(),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// `name\tsandbox\tcontainers\tcreated` lines of `docker network inspect`.
pub fn parse_network_lines(stdout: &str) -> Vec<NetworkInfo> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut f = line.trim_end_matches('\r').split('\t');
            let name = f.next()?.trim();
            let sandbox = f.next()?.trim();
            let containers = f.next()?.trim().parse().ok()?;
            let created = f.next().map(str::trim).filter(|c| !c.is_empty());
            (!name.is_empty()).then(|| NetworkInfo {
                name: name.to_string(),
                sandbox: sandbox.to_string(),
                containers,
                created_at: created.map(str::to_string),
            })
        })
        .collect()
}

/// Whether a `docker` error says a network does not exist (any more).
fn network_gone(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    e.contains("not found") || e.contains("no such network")
}

/// Inspects `names` in one `inspect` call; when that fails because a network
/// went away after it was listed (a sandbox destroyed meanwhile: `docker
/// network inspect` then exits non-zero for the whole call), inspects them
/// one by one and skips the ones that are gone. Any other failure fails.
pub async fn inspect_tolerant<F, Fut>(
    names: Vec<String>,
    inspect: F,
) -> Result<Vec<NetworkInfo>, String>
where
    F: Fn(Vec<String>) -> Fut,
    Fut: std::future::Future<Output = Result<String, String>>,
{
    match inspect(names.clone()).await {
        Ok(out) => Ok(parse_network_lines(&out)),
        Err(e) if !network_gone(&e) => Err(e),
        Err(_) => {
            let mut all = vec![];
            for name in names {
                match inspect(vec![name]).await {
                    Ok(out) => all.extend(parse_network_lines(&out)),
                    Err(e) if network_gone(&e) => {}
                    Err(e) => return Err(e),
                }
            }
            Ok(all)
        }
    }
}

#[async_trait::async_trait]
impl DockerNetworks for CliDockerNetworks {
    async fn list(&self, label: &str) -> Result<Vec<NetworkInfo>, String> {
        let filter = format!("label={label}");
        let names = self
            .docker(&[
                "network",
                "ls",
                "--filter",
                &filter,
                "--format",
                "{{.Name}}",
            ])
            .await?;
        let names: Vec<&str> = names
            .lines()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .collect();
        if names.is_empty() {
            return Ok(vec![]);
        }
        let format = format!(
            "{{{{.Name}}}}\t{{{{index .Labels \"{label}\"}}}}\t{{{{len .Containers}}}}\t{{{{.Created}}}}"
        );
        let names = names.into_iter().map(str::to_string).collect();
        inspect_tolerant(names, |names: Vec<String>| {
            let format = format.clone();
            async move {
                let mut args = vec!["network", "inspect", "--format", format.as_str()];
                args.extend(names.iter().map(String::as_str));
                self.docker(&args).await
            }
        })
        .await
    }

    async fn remove(&self, name: &str) -> Result<(), String> {
        self.docker(&["network", "rm", name]).await.map(|_| ())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetworkReapReport {
    pub listed: usize,
    pub removed: Vec<String>,
    pub kept: usize,
    pub errors: Vec<String>,
}

pub struct NetworkReaper {
    pub docker: Arc<dyn DockerNetworks>,
    /// Only networks whose sandbox label value starts with this
    /// ([`SANDBOX_NAME_PREFIX`]).
    pub sandbox_prefix: String,
    pub min_age: Duration,
}

impl NetworkReaper {
    pub fn new(docker: Arc<dyn DockerNetworks>) -> Self {
        Self {
            docker,
            sandbox_prefix: SANDBOX_NAME_PREFIX.to_string(),
            min_age: DEFAULT_MIN_AGE,
        }
    }

    pub fn with_sandbox_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.sandbox_prefix = prefix.into();
        self
    }

    pub fn with_min_age(mut self, min_age: Duration) -> Self {
        self.min_age = min_age;
        self
    }

    /// One pass at `now` (unix seconds): removes every guard sandbox network
    /// with no container that is at least `min_age` old.
    pub async fn reap(&self, now: i64) -> Result<NetworkReapReport, String> {
        let networks = self.docker.list(SANDBOX_LABEL).await?;
        let mut report = NetworkReapReport::default();
        for n in networks
            .iter()
            .filter(|n| n.sandbox.starts_with(&self.sandbox_prefix))
        {
            report.listed += 1;
            if n.containers > 0 || !old_enough(n.created_at.as_deref(), now, self.min_age) {
                report.kept += 1;
                continue;
            }
            match self.docker.remove(&n.name).await {
                Ok(()) => report.removed.push(n.name.clone()),
                Err(e) => report.errors.push(format!("{}: {e}", n.name)),
            }
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_parses_offsets_fractions_and_docker_nanos() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("2026-11-01T10:20:30Z"), Some(1_793_528_430));
        assert_eq!(
            parse_rfc3339("2026-11-01T10:20:30.123456789Z"),
            Some(1_793_528_430)
        );
        assert_eq!(
            parse_rfc3339("2026-11-01T12:20:30+02:00"),
            Some(1_793_528_430)
        );
        assert_eq!(parse_rfc3339("2026-11-01 10:20:30"), Some(1_793_528_430));
        // Go's default time format (`docker network inspect {{.Created}}`).
        assert_eq!(
            parse_rfc3339("2026-11-01 10:20:30.604243126 +0000 UTC"),
            Some(1_793_528_430)
        );
        assert_eq!(parse_rfc3339("2000-02-29T00:00:00Z"), Some(951_782_400));
        assert_eq!(parse_rfc3339("nope"), None);
        assert_eq!(parse_rfc3339("2026-13-01T00:00:00Z"), None);
    }

    #[test]
    fn network_lines_parse_and_skip_garbage() {
        let out =
            "guard-sbx-1-net\tguard-sbx-1\t0\t2026-11-01T10:00:00.5Z\r\nbad line\nn2\ts\t2\t\n";
        let got = parse_network_lines(out);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, "guard-sbx-1-net");
        assert_eq!(got[0].containers, 0);
        assert_eq!(got[1].created_at, None);
    }

    #[test]
    fn lane_run_ids_need_the_lane_alias_shape_and_matching_metadata() {
        let key = |alias: &str, run: Option<&str>| KeyInfo {
            key_id: "h".into(),
            alias: Some(alias.into()),
            metadata: run
                .map(|r| [("run_id".to_string(), r.to_string())].into())
                .unwrap_or_default(),
            ..KeyInfo::default()
        };
        let lane = |k: &KeyInfo| lane_run_id(k, is_lane_run_id);
        assert_eq!(
            lane(&key("a1-shop-1:coder:2", None)).as_deref(),
            Some("a1-shop-1")
        );
        assert_eq!(
            lane(&key("a1-shop-1:qa:1:c2:claude", Some("a1-shop-1"))).as_deref(),
            Some("a1-shop-1")
        );
        for (alias, run) in [
            ("a1-shop-1:coder:2", Some("other")),
            ("loose", None),
            ("a1-shop-1", None),
            ("a1-shop-1:lead:1", None),
            ("a1-shop-1:qa:", None),
            ("a-shop:qa:1", None),
            ("a1:qa:1", None),
            ("a1-SHOP:qa:1", None),
            ("alice:dev:1", None),
        ] {
            assert_eq!(lane(&key(alias, run)), None, "{alias}");
        }
    }

    #[tokio::test]
    async fn a_network_gone_between_ls_and_inspect_is_skipped_not_fatal() {
        let line = |n: &str| {
            format!(
                "{n}	guard-sbx-x	0	2026-11-01T10:00:00Z
"
            )
        };
        let inspect = |names: Vec<String>| async move {
            if names.iter().any(|n| n == "gone") {
                Err(
                    "docker network failed: Error response from daemon: network gone not found"
                        .to_string(),
                )
            } else {
                Ok(names.iter().map(|n| line(n)).collect::<String>())
            }
        };
        let names = vec!["n1".to_string(), "gone".to_string(), "n2".to_string()];
        let got = inspect_tolerant(names, inspect).await.unwrap();
        let got: Vec<_> = got.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(got, ["n1", "n2"]);

        // Any other failure still fails the listing.
        let down = |_: Vec<String>| async { Err::<String, _>("cannot run docker".to_string()) };
        assert!(inspect_tolerant(vec!["n1".into()], down).await.is_err());
    }
}
