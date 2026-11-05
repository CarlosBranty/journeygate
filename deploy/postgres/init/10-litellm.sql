-- First start of the postgres volume only: the database of the LiteLLM
-- gateway (profile "agents"; spend logs and virtual keys). Owned by the same
-- login as guard's database.
CREATE DATABASE litellm;
