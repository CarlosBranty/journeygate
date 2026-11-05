-- Phase 4 agent lane: orchestrator memory (bi-temporal facts + human-approval
-- candidates), the (ticket, role) task queue, agent runs and gateway spend.
-- Contracts: docs/contracts/memory.md, docs/contracts/tokens.md,
-- guard-core/src/ports/{memory_facts,task_queue,model_gateway}.rs.
--
-- Roles (cluster-wide, NOLOGIN group roles; deploy grants membership to login users):
--   guard_orchestrator  : SELECT/INSERT/UPDATE on every table below (never DELETE).
--   guard_memory_reader : SELECT on memory_facts only. `guard mcp` (guard_history)
--                         and the facts-brief builder connect as a member of it.
-- The migration creates the roles when it may (CREATEROLE) and grants on the
-- tables of the schema it runs in; without CREATEROLE it skips role creation
-- with a NOTICE and the DBA creates them (memory.md, "Database roles").

-- ---------------------------------------------------------------- roles

DO $$
DECLARE
    r text;
BEGIN
    FOREACH r IN ARRAY ARRAY['guard_orchestrator', 'guard_memory_reader'] LOOP
        IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = r) THEN
            BEGIN
                EXECUTE format('CREATE ROLE %I NOLOGIN', r);
            EXCEPTION
                WHEN duplicate_object OR unique_violation THEN NULL;  -- concurrent migration
                WHEN insufficient_privilege THEN
                    RAISE NOTICE 'role % not created (no CREATEROLE); create it out of band', r;
            END;
        END IF;
    END LOOP;
END
$$;

-- ---------------------------------------------------------------- memory

CREATE TABLE memory_facts (
    id                bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo              text        NOT NULL CHECK (repo <> ''),
    scope             text        NOT NULL CHECK (scope IN ('repo', 'area', 'journey')),
    scope_key         text        CHECK (scope_key <> ''),
    topic_key         text        NOT NULL
                                  CHECK (topic_key ~ '^[a-z0-9][a-z0-9._/-]*$' AND length(topic_key) <= 120),
    body              text        NOT NULL CHECK (octet_length(body) BETWEEN 1 AND 1024),
    commit_sha        text        NOT NULL CHECK (commit_sha ~ '^[0-9a-f]{7,64}$'),
    run_id            text,
    evidence_uri      text,
    source            text        NOT NULL CHECK (source IN ('gate_result', 'failure_fingerprint',
                                  'flaky_history', 'fix_commit', 'approved_decision', 'approved_candidate')),
    -- valid time
    valid_from        timestamptz NOT NULL,
    invalid_at        timestamptz,
    -- transaction time
    recorded_at       timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now(),
    review_after      timestamptz,
    status            text        NOT NULL DEFAULT 'active'
                                  CHECK (status IN ('active', 'needs_review', 'invalidated')),
    review_reason     text,
    supersedes        bigint      REFERENCES memory_facts (id),
    referenced_paths  text[]      NOT NULL DEFAULT '{}',
    CHECK ((scope = 'repo') = (scope_key IS NULL)),
    CHECK ((status = 'invalidated') = (invalid_at IS NOT NULL)),
    CHECK (invalid_at IS NULL OR invalid_at >= valid_from)
);

-- One live (non-invalidated) fact per topic; a new version supersedes.
CREATE UNIQUE INDEX memory_facts_live_topic_idx
    ON memory_facts (repo, scope, coalesce(scope_key, ''), topic_key)
    WHERE status <> 'invalidated';
CREATE INDEX memory_facts_repo_idx ON memory_facts (repo, id);
-- Staleness job: facts referencing files a commit touched.
CREATE INDEX memory_facts_paths_idx ON memory_facts USING gin (referenced_paths);
CREATE INDEX memory_facts_review_idx ON memory_facts (review_after) WHERE status = 'active';

-- Append-only: what a fact claims never changes and rows are never deleted.
-- Only status, invalid_at, review_after and review_reason move.
CREATE FUNCTION memory_facts_append_only() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'memory_facts rows are never deleted (invalidate instead)';
    END IF;
    IF (NEW.repo, NEW.scope, NEW.scope_key, NEW.topic_key, NEW.body, NEW.commit_sha,
        NEW.run_id, NEW.evidence_uri, NEW.source, NEW.valid_from, NEW.recorded_at,
        NEW.supersedes, NEW.referenced_paths)
       IS DISTINCT FROM
       (OLD.repo, OLD.scope, OLD.scope_key, OLD.topic_key, OLD.body, OLD.commit_sha,
        OLD.run_id, OLD.evidence_uri, OLD.source, OLD.valid_from, OLD.recorded_at,
        OLD.supersedes, OLD.referenced_paths) THEN
        RAISE EXCEPTION 'memory_facts claims are immutable; record a new fact that supersedes %', OLD.id;
    END IF;
    IF OLD.status = 'invalidated' AND NEW.status <> 'invalidated' THEN
        RAISE EXCEPTION 'an invalidated fact cannot be revived; record a new fact';
    END IF;
    NEW.updated_at := now();
    RETURN NEW;
END
$$;

CREATE TRIGGER memory_facts_append_only
    BEFORE UPDATE OR DELETE ON memory_facts
    FOR EACH ROW EXECUTE FUNCTION memory_facts_append_only();

-- Agent proposals waiting for a human (never read by agents).
CREATE TABLE memory_fact_candidates (
    id           bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo         text        NOT NULL CHECK (repo <> ''),
    fact         jsonb       NOT NULL,   -- a NewFact (guard_core::ports::memory_facts)
    proposed_by  text        NOT NULL,   -- <runtime>:<role>
    run_id       text,
    created_at   timestamptz NOT NULL DEFAULT now(),
    decision     text        NOT NULL DEFAULT 'pending'
                             CHECK (decision IN ('pending', 'approved', 'rejected')),
    decided_by   text,
    decided_at   timestamptz,
    reason       text,
    fact_id      bigint      REFERENCES memory_facts (id),
    CHECK ((decision = 'pending') = (decided_at IS NULL)),
    CHECK ((decision = 'approved') = (fact_id IS NOT NULL))
);

CREATE INDEX memory_fact_candidates_pending_idx
    ON memory_fact_candidates (repo, id) WHERE decision = 'pending';

-- ---------------------------------------------------------------- queue

CREATE TABLE task_queue (
    id               bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo             text        NOT NULL CHECK (repo <> ''),
    ticket           text        NOT NULL CHECK (ticket <> ''),
    role             text        NOT NULL CHECK (role IN ('qa', 'coder')),
    priority         integer     NOT NULL DEFAULT 0,
    predicted_files  text[]      NOT NULL DEFAULT '{}',
    payload          jsonb       NOT NULL DEFAULT '{}',
    state            text        NOT NULL DEFAULT 'queued'
                                 CHECK (state IN ('queued', 'running', 'succeeded', 'failed')),
    attempts         integer     NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    max_attempts     integer     NOT NULL DEFAULT 3 CHECK (max_attempts >= 1),
    run_after        timestamptz NOT NULL DEFAULT now(),
    locked_by        text,
    locked_until     timestamptz,
    last_error       text,
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now(),
    CHECK ((state = 'running') = (locked_by IS NOT NULL)),
    CHECK ((state = 'running') = (locked_until IS NOT NULL))
);

CREATE UNIQUE INDEX task_queue_active_pair_idx ON task_queue (repo, ticket, role)
    WHERE state IN ('queued', 'running');
CREATE INDEX task_queue_claim_idx ON task_queue (priority DESC, run_after, id)
    WHERE state IN ('queued', 'running');
-- Conflict check: running tasks whose predicted files overlap (&&).
CREATE INDEX task_queue_files_idx ON task_queue USING gin (predicted_files)
    WHERE state = 'running';

-- ---------------------------------------------------------------- runs

-- One row per sandboxed attempt of a (ticket, role).
CREATE TABLE agent_runs (
    id                  bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    run_id              text        NOT NULL CHECK (run_id <> ''),
    task_id             bigint      REFERENCES task_queue (id) ON DELETE SET NULL,
    repo                text        NOT NULL,
    ticket              text        NOT NULL,
    role                text        NOT NULL CHECK (role IN ('qa', 'coder')),
    attempt             integer     NOT NULL CHECK (attempt BETWEEN 1 AND 3),
    agent               text        NOT NULL,   -- [agents.<name>]
    runtime             text        NOT NULL,   -- AgentRuntime::name()
    model               text        NOT NULL,
    arm                 text,                   -- A/B arm (tokens.md)
    sandbox_id          text,
    key_alias           text,                   -- gateway virtual key alias
    base_commit         text        NOT NULL CHECK (base_commit ~ '^[0-9a-f]{7,64}$'),
    status              text        NOT NULL DEFAULT 'running'
                                    CHECK (status IN ('running', 'completed', 'failed', 'budget_exceeded',
                                    'timed_out', 'refused', 'error')),
    outcome             text        CHECK (outcome IN ('passed', 'gate_failed', 'red_first_passed',
                                    'diff_rejected', 'quality_rejected', 'runtime_error')),
    sealed_spec_sha256  text        CHECK (sealed_spec_sha256 ~ '^[0-9a-f]{64}$'),
    diff_paths          text[]      NOT NULL DEFAULT '{}',
    usage               jsonb,                  -- guard_core::ports::Usage
    cost_usd            numeric(12, 6),
    steps               integer,
    memory_reads        integer     NOT NULL DEFAULT 0,
    facts_brief_ids     bigint[]    NOT NULL DEFAULT '{}',
    events_uri          text,                   -- s3:// copy of /task/events.jsonl
    result              jsonb,                  -- /task/result.json
    started_at          timestamptz NOT NULL DEFAULT now(),
    finished_at         timestamptz,
    UNIQUE (run_id, role, attempt)
);

CREATE INDEX agent_runs_ticket_idx ON agent_runs (repo, ticket, started_at DESC);
CREATE INDEX agent_runs_arm_idx ON agent_runs (arm) WHERE arm IS NOT NULL;

-- ---------------------------------------------------------------- spend

-- Pages of LiteLLM /spend/logs/v2, upserted by request_id.
CREATE TABLE spend_records (
    request_id                   text PRIMARY KEY,
    key_alias                    text,
    run_id                       text,
    arm                          text,
    model                        text           NOT NULL,
    spend_usd                    numeric(14, 8) NOT NULL CHECK (spend_usd >= 0),
    prompt_tokens                bigint         NOT NULL DEFAULT 0 CHECK (prompt_tokens >= 0),
    completion_tokens            bigint         NOT NULL DEFAULT 0 CHECK (completion_tokens >= 0),
    cached_tokens                bigint         NOT NULL DEFAULT 0 CHECK (cached_tokens >= 0),
    cache_creation_input_tokens  bigint         NOT NULL DEFAULT 0 CHECK (cache_creation_input_tokens >= 0),
    cache_hit                    boolean,
    start_time                   timestamptz    NOT NULL,
    end_time                     timestamptz    NOT NULL,
    metadata                     jsonb          NOT NULL DEFAULT '{}',
    ingested_at                  timestamptz    NOT NULL DEFAULT now()
);

CREATE INDEX spend_records_run_idx ON spend_records (run_id, start_time);
CREATE INDEX spend_records_key_idx ON spend_records (key_alias, start_time);

-- ---------------------------------------------------------------- grants

REVOKE ALL ON memory_facts, memory_fact_candidates, task_queue, agent_runs, spend_records FROM PUBLIC;

DO $$
DECLARE
    s text := current_schema();
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'guard_orchestrator') THEN
        EXECUTE format('GRANT USAGE ON SCHEMA %I TO guard_orchestrator', s);
        EXECUTE format('GRANT SELECT, INSERT, UPDATE ON %1$I.memory_facts, %1$I.memory_fact_candidates,
                        %1$I.task_queue, %1$I.agent_runs, %1$I.spend_records TO guard_orchestrator', s);
        EXECUTE format('GRANT USAGE ON %1$I.memory_facts_id_seq, %1$I.memory_fact_candidates_id_seq,
                        %1$I.task_queue_id_seq, %1$I.agent_runs_id_seq TO guard_orchestrator', s);
    END IF;
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'guard_memory_reader') THEN
        EXECUTE format('GRANT USAGE ON SCHEMA %I TO guard_memory_reader', s);
        EXECUTE format('GRANT SELECT ON %I.memory_facts TO guard_memory_reader', s);
    END IF;
END
$$;
