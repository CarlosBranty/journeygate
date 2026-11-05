-- Durable job queue (Phase 4 scheduler, claimed with FOR UPDATE SKIP LOCKED)
-- and per-test outcomes of every gate result (dashboard flaky history).
-- Semantics: guard-store/src/jobs.rs and guard-store/src/flaky.rs.

CREATE TABLE jobs (
    id            bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    queue         text        NOT NULL CHECK (queue <> ''),
    dedupe_key    text,
    payload       jsonb       NOT NULL,
    priority      integer     NOT NULL DEFAULT 0,
    status        text        NOT NULL DEFAULT 'queued'
                              CHECK (status IN ('queued', 'running', 'succeeded', 'dead')),
    attempts      integer     NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    max_attempts  integer     NOT NULL DEFAULT 3 CHECK (max_attempts >= 1),
    run_after     timestamptz NOT NULL DEFAULT now(),
    locked_by     text,
    locked_until  timestamptz,
    last_error    text,
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    CHECK ((status = 'running') = (locked_by IS NOT NULL)),
    CHECK ((status = 'running') = (locked_until IS NOT NULL))
);

-- One active job per (queue, dedupe_key): the (ticket, role) pair.
CREATE UNIQUE INDEX jobs_active_dedupe_idx ON jobs (queue, dedupe_key)
    WHERE dedupe_key IS NOT NULL AND status IN ('queued', 'running');

-- Claim scan: claimable rows of a queue in claim order.
CREATE INDEX jobs_claim_idx ON jobs (queue, priority DESC, run_after, id)
    WHERE status IN ('queued', 'running');

CREATE TABLE gate_test_results (
    gate_result_id  bigint  NOT NULL REFERENCES gate_results (id) ON DELETE CASCADE,
    test_id         text    NOT NULL,
    journey         text,
    status          text    NOT NULL CHECK (status IN ('passed', 'flaky', 'failed', 'skipped')),
    blocking        boolean NOT NULL,
    attempts        integer NOT NULL
);

CREATE INDEX gate_test_results_gate_idx ON gate_test_results (gate_result_id);
