use super::*;

const SAUNAS: &str = "version = 1

[app]
name = \"saunas\"
base_url = \"http://localhost:3000\"
ready_path = \"/\"

# guard does not start the app: you run it yourself (guard init --runner self).
# Detected commands, uncomment to let guard start it (needed in CI):
# start = \"docker compose -f docker-compose.yml up -d --wait postgres redis mailpit\"
# stop = \"docker compose -f docker-compose.yml down\"
#
# [[services]]
# name = \"backend\"
# cwd = \"backend\"
# start = \"bun run dev\"
# ready_url = \"http://localhost:3001/health\"
#
# [[services]]
# name = \"root\"
# cwd = \".\"
# start = \"bun run dev\"

[areas.backend]
description = \"x\"
paths = [\"backend/**\"]
";

#[test]
fn hint_lists_the_app_start_then_each_service() {
    assert_eq!(
        hint_lines(SAUNAS),
        vec![
            "guard does not start the app (no [app].start in target.toml); start it yourself:",
            "  docker compose -f docker-compose.yml up -d --wait postgres redis mailpit",
            "  backend (in backend): bun run dev",
            "  root: bun run dev",
            "then run guard up again (or uncomment those lines so guard starts them)",
        ]
    );
}

#[test]
fn hint_accepts_a_hash_without_space_and_crlf() {
    let text = format!(
        "[app]\r\n{SELF_MODE_HEADER}\r\n{SELF_MODE_MARKER}\r\n#start = \"npm run dev\"\r\n"
    );
    assert_eq!(
        hint_lines(&text),
        vec![
            "guard does not start the app (no [app].start in target.toml); start it yourself:",
            "  npm run dev",
            "then run guard up again (or uncomment those lines so guard starts them)",
        ]
    );
}

#[test]
fn hint_without_a_usable_block_is_the_generic_one() {
    let generic = vec!["start your app, then run guard up again".to_string()];
    assert_eq!(hint_lines("version = 1\n"), generic);
    // The block ends at the first line that is not a comment.
    let ended = format!("{SELF_MODE_MARKER}\n\n# start = \"npm run dev\"\n");
    assert_eq!(hint_lines(&ended), generic);
    let broken = format!("{SELF_MODE_MARKER}\n# start = npm run dev\n");
    assert_eq!(hint_lines(&broken), generic);
    let empty = format!("{SELF_MODE_MARKER}\n#\n");
    assert_eq!(hint_lines(&empty), generic);
}
