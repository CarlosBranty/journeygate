# Contract: app detection (`guard init`)

How `guard init` finds out how an app runs: its services, frameworks, package managers, start commands, ports, ready paths and compose infrastructure. The result is a `DetectionReport` that `guard init` prints as the plan, lets the user confirm, and turns into `target.toml` ([`target-toml.md`](target-toml.md), [`cli.md`](cli.md) "`guard init`").

| Item | Value |
| --- | --- |
| Implementation | `guard_core::detect` (the module that already holds the `init` helpers for languages, code host, ticket pattern and areas). Pure: no I/O, no network, no clock, no environment. Same input, same report. |
| Caller | `guard-cli`. It lists files, reads the small files the module asks for, and renders the report. |
| API | `detect::wanted_files(&files, &contents) -> Vec<String>`, `detect::detect_app(&DetectInput) -> DetectionReport`, `detect::app_plan(&DetectionReport) -> AppPlan` (the `[app]` start/stop, `base_url`, `ready_path` and `[[services]]` of the target shape below), `detect::DETECT_IGNORED_DIRS` (the ignore list below, for the CLI's walk). |
| Network | Never. Detection does not start anything or probe ports. |

## Inputs

```rust
pub struct DetectInput {
    /// Repo-relative paths, forward slashes, without the ignored directories below.
    pub files: Vec<String>,
    /// Contents of the files the module asked for (see "Files read").
    pub contents: BTreeMap<String, String>,
    /// Directory name of the repo root (fallback service name).
    pub repo_name: String,
}
```

**File list.** Inside a git repository: `git ls-files --cached --others --exclude-standard`. Outside one: a walk of the root. Both drop every path with a segment in the ignore list and stop at 50 000 files (`notes` records the truncation).

**Ignored directories** (any depth): `node_modules`, `vendor`, `target`, `dist`, `build`, `.git`, `.next`, `.nuxt`, `.svelte-kit`, `.venv`, `venv`, `__pycache__`, `bin`, `obj`, `e2e-playwright`, `src-tauri`. `src-tauri` is the desktop shell of a frontend, not a service.

**Files read.** The CLI asks `detect::wanted_files(&files, &contents) -> Vec<String>` which files to read, reads them (each at most 256 KiB, larger files are skipped and noted), and calls it again until it returns no new path (at most 3 rounds). Only paths on this allowlist are ever returned, so the CLI never opens anything else:

| Pattern (in a service root, see below) | Why |
| --- | --- |
| `package.json`, `composer.json`, `pyproject.toml`, `requirements.txt`, `go.mod`, `Cargo.toml`, `Gemfile`, `*.csproj` | Manifests. |
| `Properties/launchSettings.json` | .NET ports. |
| `next.config.*`, `vite.config.*`, `nuxt.config.*`, `svelte.config.*`, `astro.config.*`, `remix.config.*` | Framework and port overrides. |
| `.env.example` | `PORT=`. **Only this exact name.** `.env`, `.env.local`, `.env.*` and every other dotenv file are never listed by `wanted_files` and never read. |
| `docker-compose*.yml`, `docker-compose*.yaml`, `compose*.yml`, `compose*.yaml` (root and service roots) | Infrastructure. |
| Backend sources: the entry file of the `dev`/`start` script (`bun run --watch src/index.ts` -> `src/index.ts`; `node dist/x.js` -> no), the files it imports with a relative specifier (one level, `./server` resolved against `.ts`, `.js`, `.mjs`, `/index.ts`, `/index.js`), and the conventional files `src/{index,main,server,app}.{ts,js,mjs}`, `{index,main,server,app}.{ts,js,mjs}`, `src/config/env.{ts,js}`, `src/env.{ts,js}`, `config/env.{ts,js}`, `main.py`, `app/main.py`, `app.py`, `main.go`, `cmd/*/main.go`, `src/main.rs`, `routes/web.php`, `routes/api.php`. At most 12 source files per service. | Literal ports and health routes. |

Lockfile presence (`bun.lock`, `bun.lockb`, `pnpm-lock.yaml`, `yarn.lock`, `package-lock.json`) and `artisan`, `manage.py`, `next.config.*` presence are read from the file list, not from contents.

## Service roots

A service root is the repo root or a directory at depth 1 or 2 that directly holds a manifest: `package.json`, `composer.json`, `pyproject.toml`, `requirements.txt`, `manage.py`, `go.mod`, `Cargo.toml`, `Gemfile` or a `*.csproj`. Then:

- A root whose framework resolves to `unknown` **and** that has no start command is not a service; it is reported in `skipped` with the reason. A `package.json` without `scripts.dev`/`scripts.start` and without a known framework (a tooling-only package) is such a root.
- A `Cargo.toml` is a service only with a web dependency (`axum`, `actix-web`, `rocket`, `warp`, `poem`, `salvo`); a `go.mod` always is (low confidence without a `net/http`-style listen literal).
- A root that is an npm/pnpm/yarn workspace container (`package.json` with `workspaces`, or `pnpm-workspace.yaml`) is a service only when its own scripts have `dev`/`start`.

**Service name**: the root's directory name (the repo name for the root), lower-cased, every character outside `[a-z0-9_-]` replaced by `-`, prefixed with `s-` when it does not start with a letter; on a collision `-2`, `-3`, ... is appended in path order.

**Order**: backends first, then `unknown`, then frontends; ties by root path. This is the `[[services]]` order, so APIs are up before the frontends that call them.

## Framework and kind

First match wins, per root.

| Framework | Kind | Evidence that selects it |
| --- | --- | --- |
| `laravel` | backend | `artisan` file and `composer.json` requiring `laravel/framework`. Wins over a `package.json` (Vite assets) in the same root. |
| `symfony` | backend | `composer.json` requiring `symfony/framework-bundle`. |
| `django` | backend | `manage.py`, or `django` in `pyproject.toml`/`requirements.txt`. |
| `fastapi` | backend | `fastapi` in `pyproject.toml`/`requirements.txt`. |
| `flask` | backend | `flask` in `pyproject.toml`/`requirements.txt`. |
| `rails` | backend | `Gemfile` with `gem "rails"` / `gem 'rails'`. |
| `dotnet` | backend | a `*.csproj` with `Sdk="Microsoft.NET.Sdk.Web"`. |
| `next` | frontend | `next` in `dependencies`/`devDependencies`, or a `next.config.*`. |
| `nuxt` | frontend | `nuxt`. |
| `remix` | frontend | any `@remix-run/*`, or `@react-router/dev`. |
| `sveltekit` | frontend | `@sveltejs/kit`. |
| `astro` | frontend | `astro`. |
| `nestjs` | backend | `@nestjs/core`. |
| `elysia` | backend | `elysia`. |
| `hono` | backend | `hono`. |
| `fastify` | backend | `fastify`. |
| `express` | backend | `express`. |
| `vite` | frontend | `vite` (checked after the meta-frameworks, which depend on it). |
| `go` | backend | `go.mod`. |
| `rust` | backend | `Cargo.toml` with a web dependency. |
| `unknown` | unknown | none of the above. |

## Package manager (Node roots)

Lockfiles in the root, else in the nearest ancestor up to the repo root: `bun.lock`/`bun.lockb` -> `bun`, `pnpm-lock.yaml` -> `pnpm`, `yarn.lock` -> `yarn`, `package-lock.json` -> `npm`. Several: prefer `bun` > `pnpm` > `yarn` > `npm`, confidence `medium`, and the conflict is evidence (`acme-frontend: bun.lock and package-lock.json; using bun`; the repo root is named `repo root`). None: `npm`, confidence `low`, evidence `no lockfile`. Non-Node roots have `package_manager: null`.

## Start command (local development)

| Framework | Command |
| --- | --- |
| Node frameworks | `scripts.dev` exists -> `<pm> run dev` (`bun run dev`, `npm run dev`, `pnpm run dev`, `yarn run dev`); else `scripts.start` -> `<pm> run start`; else no command (`skipped` unless the framework is known, then `low` confidence `npx <framework cli> dev`: `next dev`, `nuxt dev`, `vite`, `astro dev`). |
| `laravel` | `php artisan serve --port <port>` |
| `symfony` | `php -S 127.0.0.1:<port> -t public` |
| `django` | `python manage.py runserver <port>` |
| `fastapi` | `uvicorn <module>:app --reload --port <port>`, `<module>` = `main` for `main.py`, `app.main` for `app/main.py`; `low` confidence otherwise. |
| `flask` | `flask run --port <port>` |
| `rails` | `bin/rails server -p <port>` |
| `dotnet` | `dotnet run` |
| `go` | `go run .` (`go run ./cmd/<name>` when only `cmd/<name>/main.go` exists) |
| `rust` | `cargo run` |

Confidence is `high` for a script taken from `package.json` or a framework command whose framework evidence is `high`, else `medium`.

## Port

First rule that yields a port wins; each rule names its confidence.

| # | Source | Example evidence | Confidence |
| --- | --- | --- | --- |
| 1 | `-p <n>`, `-p=<n>`, `--port <n>`, `--port=<n>` in the chosen script | `frontend/package.json scripts.dev: next dev -p 3100` | high |
| 2 | `applicationUrl` in `Properties/launchSettings.json` (first `http://` URL of the first profile) | `api/Properties/launchSettings.json applicationUrl http://localhost:5080` | high |
| 3 | A literal in the backend sources: `.listen(<n>`, `.listen({ port: <n>`, `PORT || <n>`, `PORT ?? <n>` (number or quoted; `PORT` as a whole word, so `DB_PORT || 5432` does not count), `.default(<n>)` on a line naming the word `PORT`/`port` (not `export`, `support`, `DB_PORT`), `ListenAndServe(":<n>"`, `Addr: ":<n>"`, `bind("0.0.0.0:<n>")` | `acme-backend/src/config/env.ts:9 PORT || '3001'` | high |
| 4 | Only when rule 3 finds nothing in any scanned file: a bare `port: <n>` key in the backend sources (it also matches database and cache configs), skipping the well-known infrastructure ports 1025, 1433, 1521, 2181, 3306, 5432, 5672, 6379, 9042, 9092, 9200, 11211 and 27017 | `api/lib/http/listen.ts:2 port: 7070` | medium |
| 5 | `PORT=<n>` in the root's `.env.example` | `api/.env.example:3 PORT=4000` | medium |
| 6 | `server.port` literal in `vite.config.*` / `devServer.port` in `nuxt.config.*` | `web/vite.config.ts:7 port: 5174` | high |
| 7 | Framework default: `next`, `nuxt`, `remix` 3000; `vite`, `sveltekit` 5173; `astro` 4321; `laravel`, `django`, `fastapi`, `symfony` 8000; `flask` 5000; `rails` 3000; `dotnet` 5000; `elysia`, `hono`, `express`, `fastify`, `nestjs` 3000; `go`, `rust` 8080; `unknown` 3000 (`no port found; using 3000`) | `next default port 3000` | medium (frontends and Laravel/Django), low (everything else) |

Only integers in `1..=65535` count. Two services with the same port: both keep it, both drop to `low`, and `notes` gets `port 3000 claimed by backend and frontend`.

## Ready path

| Kind | Rule | Confidence |
| --- | --- | --- |
| frontend | `/` | high |
| backend | `/health` or `/healthz` when that string literal appears as a route in the scanned sources (`.get('/health'`, `Route::get('/health'`, `@app.get("/health")`, `path("health/"`, `MapGet("/health"`, `HandleFunc("/healthz"`); `/healthz` wins if both. A Laravel route in `routes/api.php` is served under `/api`, so it becomes `/api/health` | high |
| backend | otherwise `/` | low |
| unknown | `/` | low |

`ready_url = "http://localhost:" + port + ready_path`.

## Compose files

**Considered**: `docker-compose*.yml|yaml` and `compose*.yml|yaml` at the root and in service roots.

**Ignored**: a file whose name (case-insensitive) contains `prod`, `production`, `dokploy`, `deploy` or `staging`. Each ignored file is listed in `ignored_compose_files` with the matched word (`acme-backend/docker-compose.dokploy.yml: production compose file (dokploy)`).

**Parser**: a deliberate line-based subset of YAML, with no YAML crate. It reads the top-level `services:` mapping and, per service, `image:` (scalar), `build:` (scalar context, or a mapping with `context:`) and `ports:` (short syntax `"H:C"`, `"IP:H:C"`, `"H:C/proto"`, `"C"`; long syntax `published:`/`target:`). It also reads `depends_on` (list or mapping form) for the role rules and a scalar `command:` (only quoted in notes, never run). Comments, quotes and both indentation widths are handled; anchors, merges, `extends`, `${VAR}` interpolation in ports and multi-document files are not: a port the parser cannot read (`"${APP_PORT:-8000}:80"`) is dropped and gets a `notes` entry (`<file> service nginx: port "${APP_PORT:-8000}:80" is not readable`); the rest of the service is kept. A file without `services:` is ignored with a note. Because names and paths end up in the generated `[app].start`, a compose file whose path has a character outside `[A-Za-z0-9_./-]` is ignored (`<file>: path has characters guard does not put in a shell command; ignored`), and a service whose name does not match compose's own `[a-zA-Z0-9][a-zA-Z0-9_.-]*` is dropped (`<file> service <name>: not a valid compose service name; ignored`). The same path rule applies to the `go run ./<cmd dir>` start command (an unsafe `cmd/*` directory falls back to `go run .`).

**Roles** per compose service:

| Role | Rule |
| --- | --- |
| `infra` | The image name (registry and tag stripped, last path segment, e.g. `timescale/timescaledb:latest-pg16` -> `timescaledb`) is one of `postgres`, `postgis`, `mysql`, `mariadb`, `mongo`, `mongodb`, `redis`, `valkey`, `keydb`, `memcached`, `rabbitmq`, `kafka`, `zookeeper`, `nats`, `mailpit`, `mailhog`, `minio`, `elasticsearch`, `opensearch`, `timescaledb`, `clickhouse`, `localstack`, `azurite`. |
| `native` | It has a `build` context (resolved against the compose file's directory, normalised) equal to a detected service root. Checked first. That service gets `also_in_compose: "<compose service>"` (the first such compose service) and its `start` evidence gets `<file> service <name> builds <root>`; the native dev command stays the chosen start. Every further compose service building the same root (a queue worker next to the web server) keeps role `native` but is not started: `notes` gets `compose service <name> also builds <root>, which runs natively as <first>; it is not started (command: <command>): add a [[services]] entry if the app needs it` (the `(command: ...)` part only when a scalar `command:` was read), in file order among the `other` notes. |
| `infra` (dependency) | An image-only service (no `build`) whose `depends_on` is non-empty and lists only services that are `infra` by image, such as a vendor server on its own database (`traccar` on `traccar-db`). The `start` evidence gets `<file> service traccar depends only on infrastructure (traccar-db)`. |
| `other` | Anything else: a proxy in front of a built service (`nginx` with `depends_on: [laravel]`), an image without dependencies. Not started by default; listed in `notes` so the user can add it in the prompt. |

**Infrastructure commands**: the compose file used is the first, by depth then name, that has at least one `infra` service (normally the root one). With `F` its repo-relative path and `S` its `infra` service names in file order:

- `start = "docker compose -f F up -d --wait S..."`
- `stop = "docker compose -f F down"`
- confidence `high`, evidence `docker-compose.yml services postgres, redis, traccar-db, traccar, mailpit` plus one line per dependency-rule service.

Other non-ignored compose files with infrastructure are noted as not used.

**Compatibility fallback**: when no service root is found and a root compose file named `docker-compose.yml`, `docker-compose.yaml`, `compose.yml` or `compose.yaml` exists, the whole stack is the app, exactly as the previous `init`: `start = "docker compose up -d --wait"`, `stop = "docker compose down -v"`. With no service root and no compose file, a root `package.json` with `scripts.start` gives `start = "npm start"` (no stop), as before.

## base_url, ready_path and the target shape

| Field | Rule |
| --- | --- |
| `base_url` | `http://localhost:<port>` of the first frontend service in the order above; else the first backend; else `http://localhost:3000` (`low`, the previous fixed value). |
| `ready_path` | That service's ready path; else `/healthz` (the previous fixed value). |
| `target_shape` | `"single"` when there is exactly one service, its root is the repo root and there is no compose infrastructure: the service's start becomes `[app].start` and no `[[services]]` is written. Otherwise `"services"`: the compose infrastructure (if any) becomes `[app].start`/`[app].stop`, every service becomes a `[[services]]` entry (`name`, `cwd` = root, `start`, `ready_url`). |

## Evidence and confidence

Every guessed value is a `Guess`: `{ "value": <value>, "confidence": "high" | "medium" | "low", "evidence": ["<path>[:<line>] <what was seen>", ...] }`. Evidence strings name repo-relative paths and, for content matches, the 1-based line: `acme-backend/src/config/env.ts:9 PORT || '3001'`. They never quote more than the matched fragment (at most 120 characters) and never contain a value from a dotenv file other than the matched `PORT=<n>`.

A service's overall `confidence` is the lowest of its `framework`, `start` and `port` confidences.

## Report shape

`DetectionReport` derives `serde::Serialize`; `guard init --dry-run` and the plan print render it, and tests compare it as JSON. Keys appear in this order; arrays are in the orders defined above. `notes` lists unreadable compose entries (in compose file order), then `other` compose services and extra `native` compose services (in compose file order), then other compose files with infrastructure, then port collisions.

The report for the acme-saas fixture (`the_acme_saas_layout_produces_the_full_report` in `guard-core/src/detect/app/tests.rs`, an illustrative multi-service repository; line numbers are the fixture's):

```json
{
  "version": 1,
  "services": [
    {
      "name": "acme-backend",
      "root": "acme-backend",
      "kind": "backend",
      "confidence": "high",
      "framework": { "value": "elysia", "confidence": "high", "evidence": ["acme-backend/package.json dependencies.elysia"]},
      "package_manager": { "value": "bun", "confidence": "high", "evidence": ["acme-backend/bun.lock"]},
      "start": { "value": "bun run dev", "confidence": "high", "evidence": ["acme-backend/package.json scripts.dev: bun run --watch src/index.ts"]},
      "port": { "value": 3001, "confidence": "high", "evidence": ["acme-backend/src/config/env.ts:3 PORT || '3001'"]},
      "ready_path": { "value": "/health", "confidence": "high", "evidence": ["acme-backend/src/app.ts:5 .get('/health'"]},
      "ready_url": "http://localhost:3001/health",
      "also_in_compose": null
    },
    {
      "name": "acme-billing",
      "root": "acme-billing",
      "kind": "backend",
      "confidence": "medium",
      "framework": { "value": "laravel", "confidence": "high", "evidence": ["acme-billing/artisan", "acme-billing/composer.json require.laravel/framework"]},
      "package_manager": null,
      "start": { "value": "php artisan serve --port 8000", "confidence": "high", "evidence": ["laravel local dev server", "docker-compose.yml service laravel builds acme-billing"]},
      "port": { "value": 8000, "confidence": "medium", "evidence": ["laravel default port 8000"]},
      "ready_path": { "value": "/api/health", "confidence": "high", "evidence": ["acme-billing/routes/api.php:5 Route::get('/health'"]},
      "ready_url": "http://localhost:8000/api/health",
      "also_in_compose": "laravel"
    },
    {
      "name": "acme-frontend",
      "root": "acme-frontend",
      "kind": "frontend",
      "confidence": "medium",
      "framework": { "value": "next", "confidence": "high", "evidence": ["acme-frontend/package.json dependencies.next", "acme-frontend/next.config.ts"]},
      "package_manager": { "value": "bun", "confidence": "medium", "evidence": ["acme-frontend: bun.lock and package-lock.json; using bun"]},
      "start": { "value": "bun run dev", "confidence": "high", "evidence": ["acme-frontend/package.json scripts.dev: next dev"]},
      "port": { "value": 3000, "confidence": "medium", "evidence": ["next default port 3000"]},
      "ready_path": { "value": "/", "confidence": "high", "evidence": ["frontend"]},
      "ready_url": "http://localhost:3000/",
      "also_in_compose": null
    }
  ],
  "skipped": [
    { "root": "acme-tracker", "reason": "no supported manifest (build.gradle)" }
  ],
  "compose": {
    "file": "docker-compose.yml",
    "services": [
      { "name": "postgres", "image": "postgres:16-alpine", "build": null, "ports": [{  "published": 5433, "target": 5432 }], "role": "infra" },
      { "name": "redis", "image": "redis:7-alpine", "build": null, "ports": [{  "published": 6380, "target": 6379 }], "role": "infra" },
      { "name": "traccar-db", "image": "timescale/timescaledb:latest-pg16", "build": null, "ports": [], "role": "infra" },
      { "name": "traccar", "image": "traccar/traccar:latest", "build": null, "ports": [{  "published": 8082, "target": 8082}, { "published": 5055, "target": 5055 }], "role": "infra" },
      { "name": "laravel", "image": null, "build": "acme-billing", "ports": [], "role": "native" },
      { "name": "nginx", "image": "nginx:alpine", "build": null, "ports": [{  "published": 8500, "target": 80 }], "role": "other" },
      { "name": "queue", "image": null, "build": "acme-billing", "ports": [], "role": "native" },
      { "name": "mailpit", "image": "axllent/mailpit", "build": null, "ports": [{  "published": 1125, "target": 1025}, { "published": 8525, "target": 8025 }], "role": "infra" }
    ],
    "start": {
      "value": "docker compose -f docker-compose.yml up -d --wait postgres redis traccar-db traccar mailpit",
      "confidence": "high",
      "evidence": [
        "docker-compose.yml services postgres, redis, traccar-db, traccar, mailpit",
        "docker-compose.yml service traccar depends only on infrastructure (traccar-db)"
      ]
    },
    "stop": { "value": "docker compose -f docker-compose.yml down", "confidence": "high", "evidence": ["docker-compose.yml"]}
  },
  "ignored_compose_files": [
    { "file": "acme-backend/docker-compose.dokploy.yml", "reason": "production compose file (dokploy)" },
    { "file": "acme-billing/docker-compose.dokploy-supabase.yml", "reason": "production compose file (dokploy)" },
    { "file": "acme-billing/docker-compose.dokploy.yml", "reason": "production compose file (dokploy)" },
    { "file": "acme-billing/docker-compose.prod.yml", "reason": "production compose file (prod)" },
    { "file": "acme-frontend/docker-compose.dokploy.yml", "reason": "production compose file (dokploy)" }
  ],
  "base_url": { "value": "http://localhost:3000", "confidence": "medium", "evidence": ["first frontend service acme-frontend"]},
  "ready_path": { "value": "/", "confidence": "high", "evidence": ["acme-frontend ready path"]},
  "target_shape": "services",
  "notes": [
    "acme-billing/docker-compose.yml service nginx: port \"${APP_PORT:-8000}:80\" is not readable",
    "compose service nginx (nginx:alpine) is not started by default",
    "compose service queue also builds acme-billing, which runs natively as laravel; it is not started (command: php artisan queue:work redis --sleep=3 --tries=3 --max-time=3600): add a [[services]] entry if the app needs it",
    "acme-billing/docker-compose.supabase.yml has infrastructure but docker-compose.yml is used",
    "acme-billing/docker-compose.yml has infrastructure but docker-compose.yml is used"
  ]
}
```

- `compose` is `null` when no compose file is considered; its `start`/`stop` are `null` when it has no `infra` service.
- `package_manager` is `null` for non-Node roots; `port` and `ready_path` are always present for a service (a default at worst).
- `version` is `1`; additive fields may be added without bumping it.

## Out of scope

Monorepo task runners (`turbo`, `nx`) beyond reading the root's own scripts, Java/Kotlin services (`pom.xml`, `build.gradle`), Kubernetes manifests, Procfiles, and probing a running app. These show up as `skipped` roots or `notes`, never as guesses.
