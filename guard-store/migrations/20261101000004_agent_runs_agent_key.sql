-- Best-of-N candidates share (run_id, role, attempt): each candidate is a
-- different [agents.<name>], so the agent name joins the unique key and every
-- candidate keeps its own agent_runs row (docs/contracts/agents-toml.md).

ALTER TABLE agent_runs DROP CONSTRAINT IF EXISTS agent_runs_run_id_role_attempt_key;
ALTER TABLE agent_runs
    ADD CONSTRAINT agent_runs_run_id_role_attempt_agent_key UNIQUE (run_id, role, attempt, agent);
