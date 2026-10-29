// sqlx::migrate! embeds the migration list at compile time but cannot watch the
// directory itself, so a newly added migration file must trigger a rebuild here.
fn main() {
    println!("cargo:rerun-if-changed=../migrations");
}
