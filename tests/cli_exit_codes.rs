use assert_cmd::Command;

#[test]
fn missing_queue_target_exits_with_usage_code() {
    let output = Command::cargo_bin("spotuify")
        .expect("spotuify binary")
        .args(["queue", "add"])
        .assert()
        .code(2)
        .get_output()
        .clone();

    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(
        stderr.contains("provide a URI or --search QUERY"),
        "usage error should explain the missing queue target, got {stderr:?}"
    );
}
