import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser()
parser.add_argument("phase", choices=["seed", "verify"])
parser.add_argument("binary", type=Path)
parser.add_argument("--runtime", type=Path, required=True)
parser.add_argument("--evidence", type=Path, required=True)
args = parser.parse_args()
runtime = args.runtime
evidence = args.evidence
env = os.environ.copy()
env.update(SPOTUIFY_FAKE_SPOTIFY="1", SPOTUIFY_INSTANCE="spotuify-index-upgrade",
           SPOTUIFY_CLIENT_ID="fake-client-id", SPOTUIFY_EXIT_WITH_PARENT=str(os.getpid()),
           SPOTUIFY_KEYCHAIN_SERVICE="spotuify-index-upgrade")
for key, suffix in {
    "RUNTIME_DIR": "runtime", "SOCKET": "runtime/daemon.sock", "DATA_DIR": "data",
    "CACHE_DIR": "cache-dir", "CONFIG_DIR": "config-dir", "LOG_DIR": "logs",
    "CACHE_DB": "cache.sqlite", "SEARCH_INDEX": "index", "ANALYTICS_DB": "analytics.sqlite",
    "CONFIG": "spotuify.toml",
}.items():
    env["SPOTUIFY_" + key] = str(runtime / suffix)
records = []

def run(*command):
    result = subprocess.run([str(args.binary), *command], env=env, capture_output=True,
                            text=True, timeout=60)
    assert result.returncode == 0, (command, result.returncode, result.stderr, result.stdout)
    value = json.loads(result.stdout)
    records.append({"command": list(command), "result": value})
    return value

result = {"phase": args.phase, "binary": str(args.binary),
          "binary_sha256": hashlib.sha256(args.binary.read_bytes()).hexdigest(),
          "runtime": str(runtime), "commands": records}
try:
    if args.phase == "seed":
        assert not runtime.exists(), "refusing to replace an existing fixture"
        remote = run("search", "luther", "--type", "track", "--source", "remote", "--format", "json")
        assert remote, "fixture must populate real provider search results"
        run("reindex", "--format", "json")
    else:
        prior = json.loads((evidence / "index-upgrade-seed.json").read_text())
        assert prior["ok"]
    local = run("search", "luther", "--type", "track", "--source", "local", "--format", "json")
    assert local, "persisted local index must be searchable"
    result["local_items"] = sorted([item["uri"], item["name"]] for item in local)
    if args.phase == "verify":
        assert result["local_items"] == prior["local_items"], "upgrade changed cached search results"
        run("doctor", "--format", "json")
    status = run("daemon", "status", "--format", "json", "--no-daemon-start")
    result["daemon_version"] = status["daemon_version"]
    result["checks_passed"] = True
finally:
    stopped = subprocess.run([str(args.binary), "daemon", "stop"], env=env, capture_output=True,
                             text=True, timeout=20)
    result["stop"] = {"exit": stopped.returncode, "stdout": stopped.stdout.strip(),
                      "stderr": stopped.stderr.strip()}
    result["ok"] = result.get("checks_passed", False) and stopped.returncode == 0
    result["index_metadata"] = json.loads((runtime / "index/meta.json").read_text()) if (runtime / "index/meta.json").exists() else None
    (evidence / ("index-upgrade-" + args.phase + ".json")).write_text(json.dumps(result, indent=2) + "\n")
assert stopped.returncode == 0
print(json.dumps({key: result[key] for key in ["phase", "ok", "daemon_version", "local_items", "stop"]}))
