//! A database saved and loaded again answers a saved query without running it.

use ide_db::{
    RootDatabase,
    base_db::{EditionedFileId, SourceDatabase},
    executions,
};
use test_fixture::WithFixture;

#[test]
fn a_loaded_database_does_not_parse_again() {
    // SAFETY: the only test of this binary, and no other thread is running yet.
    unsafe { std::env::set_var("RUSTD_TRACE", "1") };
    let (mut db, file) = RootDatabase::with_single_file(
        r#"
fn main() { let x = 1; }
fn broken( {}
"#,
    );
    let before = file.parse(&db);
    assert!(!before.errors().is_empty());
    let ran: Vec<String> = executions::take(&db).into_iter().map(|(query, _)| query).collect();
    assert_eq!(ran, ["EditionedFileId::parse_"]);

    let bytes = db.save(&[]).unwrap();
    drop(db);
    let (db, _paths) = RootDatabase::load(&bytes).unwrap();

    let file = EditionedFileId::current_edition(&db, file.file_id(&db));
    let after = file.parse(&db);
    assert_eq!(format!("{:#?}", after.tree()), format!("{:#?}", before.tree()));
    assert_eq!(after.errors(), before.errors());
    assert_eq!(executions::take(&db), []);
}
