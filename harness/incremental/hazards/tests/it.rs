use std::process::Command;

#[test]
fn library_from_outside() {
    assert_eq!(hazards::add(1, 2), 3);
}

#[test]
fn runs_the_program() {
    let out = Command::new(env!("CARGO_BIN_EXE_hazards")).arg("it").output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hazards it\n");
}

#[test]
fn shapes_from_outside() {
    assert_eq!(hazards::shapes::first_area(), 4);
}

#[test]
fn snapshot() {
    let expected = std::fs::read_to_string("fixtures/read.txt").unwrap();
    assert_eq!(expected.trim(), "runtime");
}
