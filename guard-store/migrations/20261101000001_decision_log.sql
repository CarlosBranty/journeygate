-- Decision log and gate history (docs/contracts/decision-log.md).
-- Append-only tables; calibration reads the latest row per ticket.

CREATE TABLE triage_decisions (
    id                         bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    ticket_key                 text        NOT NULL,
    decided_at                 timestamptz NOT NULL,
    recorded_at                timestamptz NOT NULL DEFAULT now(),
    mode                       text        NOT NULL CHECK (mode IN ('shadow', 'active')),
    engine_model               text,
    engine_error               text,
    permutation_seed           bigint,
    decided_lane               text        NOT NULL CHECK (decided_lane IN ('agent', 'human')),
    decided_reasons            text[]      NOT NULL DEFAULT '{}',
    effective_lane             text        NOT NULL CHECK (effective_lane IN ('agent', 'human')),
    effective_reasons          text[]      NOT NULL DEFAULT '{}',
    -- Denormalized answers for calibration queries (NULL when the engine failed).
    area                       text,
    area_confidence            double precision,
    kind                       text CHECK (kind IN ('bugfix', 'minor_feature', 'refactor', 'architecture')),
    kind_confidence            double precision,
    risk_score                 double precision,
    risk_confidence            double precision,
    clear_e2e_criteria         double precision,
    touches_money_or_security  double precision,
    agent_suitable             double precision,
    changes_existing_flow      double precision,
    -- Full TriageDecision JSON (state, answers incl. raw, facts, thresholds, lanes).
    decision                   jsonb       NOT NULL,
    CHECK ((engine_model IS NULL) = (area IS NULL)),
    CHECK (engine_model IS NOT NULL OR engine_error IS NOT NULL)
);

CREATE INDEX triage_decisions_ticket_idx ON triage_decisions (ticket_key, decided_at DESC, id DESC);
CREATE INDEX triage_decisions_decided_at_idx ON triage_decisions (decided_at DESC);

CREATE TABLE human_labels (
    id           bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    ticket_key   text        NOT NULL,
    labeled_at   timestamptz NOT NULL,
    recorded_at  timestamptz NOT NULL DEFAULT now(),
    area         text,
    kind         text CHECK (kind IN ('bugfix', 'minor_feature', 'refactor', 'architecture')),
    lane         text CHECK (lane IN ('agent', 'human')),
    flow_change  boolean,
    labels       text[]      NOT NULL DEFAULT '{}',
    source       text        NOT NULL CHECK (source IN ('jira_webhook', 'manual'))
);

CREATE INDEX human_labels_ticket_idx ON human_labels (ticket_key, labeled_at DESC, id DESC);

-- Latest decision per ticket next to the latest human label of that ticket.
CREATE VIEW calibration AS
SELECT d.id          AS decision_id,
       d.ticket_key,
       d.decided_at,
       d.mode,
       d.decision,
       h.id          AS human_label_id,
       h.labeled_at,
       h.area        AS human_area,
       h.kind        AS human_kind,
       h.lane        AS human_lane,
       h.flow_change AS human_flow_change,
       h.labels      AS human_labels,
       h.source      AS human_source
FROM (
    SELECT DISTINCT ON (ticket_key) *
    FROM triage_decisions
    ORDER BY ticket_key, decided_at DESC, id DESC
) d
LEFT JOIN LATERAL (
    SELECT *
    FROM human_labels l
    WHERE l.ticket_key = d.ticket_key
    ORDER BY l.labeled_at DESC, l.id DESC
    LIMIT 1
) h ON true;

CREATE TABLE gate_results (
    id           bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    received_at  timestamptz NOT NULL DEFAULT now(),
    repo         text        NOT NULL DEFAULT '',
    run_id       text        NOT NULL,
    provider     text CHECK (provider IN ('github', 'gitlab')),
    pr_number    bigint,
    ticket_key   text,
    head_sha     text        NOT NULL,
    base_sha     text,
    verdict      text        NOT NULL CHECK (verdict IN ('green', 'red')),
    regressions  integer     NOT NULL DEFAULT 0,
    report       jsonb       NOT NULL,
    outcome      jsonb       NOT NULL,
    UNIQUE (repo, run_id)
);

CREATE INDEX gate_results_received_idx ON gate_results (received_at DESC, id DESC);
CREATE INDEX gate_results_pr_idx ON gate_results (repo, pr_number, received_at DESC)
    WHERE pr_number IS NOT NULL;
