// `sqlx::migrate!` embeds `migrations/` at compile time; rebuild when a
// migration is added or changed (sqlx-cli `migrate build-script`).
fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
