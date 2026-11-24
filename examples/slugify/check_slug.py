# Acceptance check, written by the human. The agent never sees or edits it.
from slug import slugify

cases = {
    "Hello, World!": "hello-world",
    "  Crème brûlée  ": "creme-brulee",
    "a--b__c": "a-b-c",
    "2026 Q4 / Plan": "2026-q4-plan",
    "!!!": "",
    "": "",
    "Ünïcödé Ñame": "unicode-name",
    "MiXeD123case": "mixed123case",
    "Søren": "soren",
}
failed = [(raw, want, slugify(raw)) for raw, want in cases.items() if slugify(raw) != want]
for raw, want, got in failed:
    print(f"FAIL slugify({raw!r}) = {got!r}, want {want!r}")
print(f"{len(cases) - len(failed)}/{len(cases)} cases pass")
raise SystemExit(1 if failed else 0)
