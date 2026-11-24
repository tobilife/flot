# slugify

Write `slug.py` exposing `slugify(text: str) -> str`:

- lowercase ASCII letters and digits are kept
- any run of other characters becomes a single `-`
- no leading or trailing `-`
- accented Latin letters are reduced to their base letter (é -> e)
- Latin letters with no Unicode decomposition map by table: ø -> o, æ -> ae, ß -> ss, đ -> d, ł -> l
- the empty string, or a string with nothing keepable, returns ""

Examples:
  slugify("Hello, World!")      == "hello-world"
  slugify("  Crème brûlée  ")   == "creme-brulee"
  slugify("a--b__c")            == "a-b-c"
  slugify("2026 Q4 / Plan")     == "2026-q4-plan"
  slugify("!!!")                == ""
  slugify("Søren Straße")       == "soren-strasse"
