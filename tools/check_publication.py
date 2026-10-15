"""Check publication privacy without printing matched values. Python stdlib only."""

from pathlib import Path
import argparse
import hashlib
import json
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
PROFILE = re.compile(r"(?i)\b[A-Z]:(?:\\+|/)+Users(?:\\+|/)+([^\\/\s\"'<>:]+)")
ABSOLUTE = re.compile(r"(?i)\b[A-Z]:(?:\\+|/)+((?:Program Files(?: \(x86\))?)|[^\\/\s\"'<>:`]+)")
TOKEN = re.compile(
    r"(?<![\w-])(?:sk-(?:ant-|proj-|svcacct-)?[A-Za-z0-9_-]{24,}"
    r"|gh[pousr]_[A-Za-z0-9]{24,}|github_pat_[A-Za-z0-9_]{30,}"
    r"|xox[baprs]-[A-Za-z0-9-]{20,}|AKIA[A-Z0-9]{16})"
)
PRIVATE_KEY = re.compile(
    r"-----BEGIN (?:RSA |EC |DSA |OPENSSH |ENCRYPTED )?PRIVATE KEY-----"
    r"\s+[A-Za-z0-9+/=]{32,}"
)
BEARER_UUID = re.compile(
    r"(?i)Bearer\s+[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}"
)
PRIVATE_SCREENSHOT = "docs/evidence/profile-backup-native.png"
SENSITIVE_SUFFIXES = {".db", ".sqlite", ".sqlite3", ".pfx", ".p12", ".pem", ".key", ".kdbx", ".dmp", ".bundle", ".alfredbackup"}
PUBLIC_SYSTEM_ROOTS = {"windows", "program files", "program files (x86)", "programdata", "nvm4w", "temp", "example", "ejemplo", "proyecto", "workspace", "sandbox", "data"}
PUBLIC_TEST_USERS = {"me", "user", "test", "fixture", "alice", "bob", "public", "default"}
DOCUMENTED_EXAMPLES = {
    "docs/files-and-watches.md": {"mis"},
    "docs/git-local.md": {"proyectos"},
    "docs/native-helper-paths.md": {"..."},
}
APPROVED_EXAMPLE_DIGESTS = {
    "src-tauri/src/clipboard_tests.rs": {
        "743554670c6065b3f7f13ac4f07e392f977b3556ceb7457411633c454bcbece8"
    }
}


def git(*args, input=None):
    return subprocess.run(["git", *args], cwd=ROOT, input=input, capture_output=True, check=True).stdout


def findings_for_text(text, path):
    findings = []
    fixture = bool(re.search(r"(?:test|fixture|contract)", path, re.I))
    documentation = path.startswith("docs/") or path.endswith(".md")
    for match in PROFILE.finditer(text):
        if (fixture or path == "src-tauri/src/policy.rs") and match.group(1).lower() in PUBLIC_TEST_USERS:
            continue
        findings.append(("private-user-profile", text.count("\n", 0, match.start()) + 1))
    if documentation:
        for match in ABSOLUTE.finditer(text):
            if match.group(1).lower() not in PUBLIC_SYSTEM_ROOTS | DOCUMENTED_EXAMPLES.get(path, set()):
                findings.append(("absolute-host-path", text.count("\n", 0, match.start()) + 1))
    for rule, pattern in [("provider-token", TOKEN), ("private-key-material", PRIVATE_KEY), ("literal-bearer-uuid", BEARER_UUID)]:
        for match in pattern.finditer(text):
            if hashlib.sha256(match.group().encode()).hexdigest() in APPROVED_EXAMPLE_DIGESTS.get(path, set()):
                continue
            if fixture and any(word in match.group().lower() for word in ["fixture", "fictional", "example", "synthetic", "not-a-real", "not_a_real"]):
                continue
            findings.append((rule, text.count("\n", 0, match.start()) + 1))
    return sorted(set(findings))


def inspect(data, path, revision=None):
    findings = []
    name = Path(path).name.lower()
    if path == PRIVATE_SCREENSHOT or Path(path).suffix.lower() in SENSITIVE_SUFFIXES or (name.startswith(".env") and name != ".env.example"):
        findings.append({"rule": "private-file-type", "path": path})
    try:
        encoding = "utf-16" if data[:2] in [b"\xff\xfe", b"\xfe\xff"] else "utf-8-sig"
        text = data.decode(encoding)
    except UnicodeError:
        return findings
    if "\0" in text or path.startswith("assets/licenses/"):
        return findings
    for rule, line in findings_for_text(text, path):
        findings.append({"rule": rule, "path": path, "line": line, **({"revision": revision} if revision else {})})
    return findings


def self_test():
    private_path = "C:" + "/Users/ExamplePerson/Desktop/report.txt"
    assert findings_for_text(private_path, "docs/evidence/sample.json")
    assert findings_for_text(json.dumps(private_path), "docs/evidence/sample.json")
    assert not findings_for_text("<USERPROFILE>/Desktop/report.txt", "docs/evidence/sample.json")
    token = "sk-" + "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6"
    assert any(rule == "provider-token" for rule, _ in findings_for_text(token, "src/config.ts"))
    assert not findings_for_text("sk-" + "fixture-not-a-real-provider-token", "tests/fixture.ts")
    assert not findings_for_text("C:" + "/Users/me/Documents", "tests/fixture.rs")
    assert findings_for_text("D:" + "/PrivateWorkspace/Project", "docs/evidence/sample.json")
    assert inspect(b"fixture", PRIVATE_SCREENSHOT)
    assert findings_for_text("C:" + "/Mis Documentos/item.txt", "docs/evidence/sample.json")
    print(json.dumps({"selfTest": "PASS", "cases": 9}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--history", action="store_true", help="Also scan every reachable blob and commit identity")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return 0
    findings = []
    files = {p for p in git("ls-files", "--cached", "--others", "--exclude-standard", "-z").decode("utf8").split("\0") if p}
    for path in sorted(files):
        file = ROOT / path
        if file.is_file():
            findings.extend(inspect(file.read_bytes(), path))
    versions = 0
    commits = 0
    if args.history:
        objects = git("rev-list", "--objects", "--all").decode("utf8").splitlines()
        batch = subprocess.Popen(["git", "cat-file", "--batch"], cwd=ROOT, stdin=subprocess.PIPE, stdout=subprocess.PIPE)
        try:
            for line in objects:
                oid, _, path = line.partition(" ")
                batch.stdin.write((oid + "\n").encode())
                batch.stdin.flush()
                header = batch.stdout.readline().split()
                data = batch.stdout.read(int(header[2]))
                batch.stdout.read(1)
                if header[1] == b"blob":
                    versions += 1
                    findings.extend(inspect(data, path, oid))
                elif header[1] == b"commit":
                    commits += 1
                    for kind, email in re.findall(rb"^(author|committer) .+ <([^>]+)>", data, re.M):
                        if not email.lower().endswith(b"@users.noreply.github.com"):
                            findings.append({"rule": "non-noreply-commit-identity", "kind": kind.decode(), "revision": oid})
        finally:
            batch.stdin.close()
            batch.stdout.close()
            batch.wait()
    result = {"status": "PASS" if not findings else "FAIL", "currentFiles": len(files), "historyBlobVersions": versions, "historyCommits": commits, "findings": findings, "scope": "Privacy and common secret formats. Complement with Gitleaks and visual review of new media."}
    print(json.dumps(result, ensure_ascii=True, indent=2))
    return 1 if findings else 0


if __name__ == "__main__":
    raise SystemExit(main())
