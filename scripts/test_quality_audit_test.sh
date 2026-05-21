#!/usr/bin/env bash

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

assert_contains() {
  local haystack="$1"
  local needle="$2"
  if ! grep -Fq "${needle}" <<< "${haystack}"; then
    echo "missing expected text: ${needle}" >&2
    echo "${haystack}" >&2
    return 1
  fi
}

mkdir -p "${tmp}/repo"
cd "${tmp}/repo"
git init -q
git config user.email "ci@example.test"
git config user.name "CI"

mkdir -p scripts crates/spotuify-core/src
cp "${root}/scripts/test_quality_audit.sh" scripts/test_quality_audit.sh
cat > crates/spotuify-core/src/lib.rs <<'EOF'
#[cfg(test)]
mod tests {
    #[test]
    fn weak_test() {
        let result: Result<(), ()> = Ok(());
        let maybe = Some(1);
        assert!(result.is_ok());
        assert!(maybe.is_some());
        assert!(true);
    }
}
EOF
git add .
git commit -q -m baseline

output="$(bash scripts/test_quality_audit.sh)"
assert_contains "${output}" "Wrote target/test-quality/audit.csv"
assert_contains "$(cat target/test-quality/audit.md)" "crates/spotuify-core/src/lib.rs"

strict_fail_out="${tmp}/strict-fail.out"
if bash scripts/test_quality_audit.sh --strict >"${strict_fail_out}" 2>&1; then
  echo "strict audit should fail on weak tests" >&2
  exit 1
fi
assert_contains "$(cat "${strict_fail_out}")" "strict mode failed"

cat > crates/spotuify-core/src/lib.rs <<'EOF'
pub fn classify_count(count: usize) -> &'static str {
    match count {
        0 => "empty",
        1..=10 => "normal",
        _ => "overflow",
    }
}

#[cfg(test)]
mod tests {
    use super::classify_count;

    #[test]
    fn classify_count_distinguishes_empty_normal_and_overflow_cases() {
        assert_eq!(classify_count(0), "empty");
        assert_eq!(classify_count(3), "normal");
        assert_eq!(classify_count(99), "overflow");
    }
}
EOF

bash scripts/test_quality_audit.sh --strict >"${tmp}/strict-pass.out"
assert_contains "$(cat "${tmp}/strict-pass.out")" "Wrote target/test-quality/audit.md"

echo "test_quality_audit_test: ok"
