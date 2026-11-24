<div align="center">

[English](../../README.md) · **한국어** · [中文](README_zh.md)

# flot

### 코딩 에이전트를 위한 검증 가능한 플로우.

**에이전트는 끝났다고 말합니다. flot은 그걸 증명하는 영수증을 남깁니다.**

*flot = **flo**w + bo**t***

[![CI](https://img.shields.io/github/actions/workflow/status/tobilife/flot/ci.yml?branch=main&style=flat-square&logo=github&label=ci)](https://github.com/tobilife/flot/actions/workflows/ci.yml)
[![MIT license](https://img.shields.io/badge/license-MIT-f4c430?style=flat-square)](../../LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-dea584?style=flat-square&logo=rust)](../../Cargo.toml)
[![Linux | macOS | Windows](https://img.shields.io/badge/Linux%20%7C%20macOS%20%7C%20Windows-4493F8?style=flat-square)](../architecture.md#current-scope)
[![MCP](https://img.shields.io/badge/MCP-stdio-8A2BE2?style=flat-square)](../mcp.md)

[왜, 언제](#왜-쓰고-언제-쓰나) · [빠른 시작](#빠른-시작) · [flot의 동작 방식](#flot의-동작-방식) · [flot이 보장하는 것](#flot이-보장하는-것) · [예제 플로우](#예제-플로우) · [FAQ](#faq) · [문서](#문서)

<code>cargo install flot-cli</code>

최신 릴리스 **0.2.3**: git 리비전에 고정된 증명, 단계의 변경 범위와 샌드박스, 재시도와 후보 비교, 실행 검색, git을 통한 공유, GitHub 액션, Claude Code 플러그인, Windows 지원 · [변경 기록](../../CHANGELOG.md)

</div>

---

코딩 에이전트는 성공을 말로 보고합니다. "구현했고, 테스트 전부 통과합니다." flot은
그 말을 증거로 바꿉니다. 할 일을 작은 단계(셸 명령, 테스트, 에이전트 CLI)의 플로우로
선언하면, flot이 그것을 실행하고 단계마다 내용 주소 기반 **영수증(receipt)**을
기록하며, 모든 주장(claim)을 에이전트가 작성하지 않은 명령으로 검증합니다.

규칙 하나는 프롬프트가 아니라 런타임이 강제합니다.

> **독립된 명령이 증명하기 전까지 주장은 검증되지 않은 상태입니다.**
> 증명되지 않은 주장이 있는 실행은 절대 `passed`가 될 수 없습니다.

영수증이 의존하는 것이 바뀌지 않았으면 영수증을 재사용하므로, 두 번째 실행은 비용이
들지 않습니다. 영수증은 번들로 묶고 서명해서 다른 머신의 리뷰어에게 넘길 수 있고,
리뷰어는 에이전트나 그 호스트를 믿지 않고도 검증할 수 있습니다. flot은 바이너리 하나,
`flot.toml` 하나, `.flot/` 디렉터리 하나로 동작하며, 터미널과 MCP를 지원하는 모든
에이전트에서 쓸 수 있습니다.

## 왜 쓰고 언제 쓰나

에이전트의 "끝났습니다"는 내 코드에 대한 주장이고, 매번 손으로 확인하는 방식은 오래가지
못합니다. 테스트를 다시 돌리고 로그를 뒤져도, 무엇이 통과했는지 다른 사람에게 보여 줄 수는
없습니다. flot은 그 확인을 작업의 일부로 만듭니다. 에이전트가 플로우를 실행하고, 플로우가
각 주장을 증명하며, 통과한 결과는 나와 CI와 리뷰어가 다시 확인할 수 있는 곳에 기록됩니다.

이럴 때 효과가 큽니다.

- **에이전트가 무언가를 고치거나 만들고 된다고 말할 때.** 결과는 에이전트가 작성하지 않은
  명령으로 판정됩니다. 실제 세션에서 Claude Code는 새로 받은 `itoa` 저장소의 실패하는
  플로우를 받아, 기록된 출력에서 실패한 테스트를 찾고 잘못된 숫자 하나를 고쳐 `flot_run`에서
  `passed`를 받았습니다. 비용은 $0.42였습니다.
- **에이전트가 고치고, 확인하고, 다시 고치는 반복을 할 때.** 입력이 바뀌지 않은 단계는
  재사용되므로 확인을 반복해도 비용이 거의 없습니다. 이 저장소의 검증은 처음에는 16초, 바뀐
  것이 없으면 0.41초 걸립니다. `attempts = N`인 단계는 증명이 남긴 실패 내용을 받아 통과할
  때까지 다시 실행됩니다. 실제 실행에서 Claude Code는 검사의 실패 출력만 보고 명세에 없던
  규칙을 찾아 두 번째 시도에서 통과했고, 비용은 $0.25였습니다.
- **에이전트 여러 개가 한 변경을 맡을 때.** 리뷰어들이 비용 상한 아래에서 동시에 실행되고,
  각각의 지출이 기록됩니다. `parallel-review`의 리뷰어 세 명을 격리하자 한 번 실행 비용이
  $3.25에서 $0.42로 줄었습니다.
- **어떤 에이전트가 일을 가장 잘하는지 알고 싶을 때.** `candidates`가 있는 단계는 에이전트나
  모델마다 프로젝트 사본을 따로 만들어 실행하고, 같은 검사로 각각을 증명한 뒤, 통과한 것 중
  가장 싼 것을 남깁니다. 실제 실행에서 Claude Code와 Codex가 모두 통과했고, Codex는 토큰만
  보고하고 비용은 보고하지 않아 $0.10인 Claude가 남았습니다.
- **다른 사람이 결과를 믿어야 할 때.** `flot bundle`은 리뷰어나 다른 머신이 가져와 검증할
  파일 하나를 만들고, `flot push`와 `flot pull`은 이미 쓰고 있는 git 원격으로 실행을
  공유합니다. 누구의 영수증을 인정할지는 서명과 받는 쪽의 신뢰 목록이 정합니다.
- **약해진 검사로 풀 리퀘스트가 머지되면 안 될 때.** GitHub 액션이 CI에서 플로우를
  실행하고, 모든 증명을 기준 브랜치에 비춰 보며, 보고서를 잡 요약과 풀 리퀘스트에
  남깁니다. 필수 검사로 두면 머지를 막습니다.
- **두 실행 사이에 무엇이 바뀌었는지 알고 싶을 때.** `flot diff`는 영수증이나 상태가 바뀐
  단계를 하나하나 짚어 주고, `flot search`는 출력, 주장, 한계에 어떤 메시지가 나온 실행을
  최신 순으로 찾아 줍니다.

이럴 때는 맞지 않습니다.

- 명령을 한 번 돌리고 기록이 필요 없을 때
- 믿을 수 없는 코드를 격리해야 할 때: 단계는 `sandbox`를 요청하지 않는 한 사용자의
  권한으로 실행되고(Linux에서는 네트워크 차단, 지정한 경로 아래에만 쓰기), 요청해도 사용자가
  읽을 수 있는 것은 읽을 수 있습니다.
- CI 서비스가 필요할 때: flot은 CI나 터미널 안에서 돌아가며 그것들을 대체하지 않습니다.

## 빠른 시작

### 설치

[Rust](https://rustup.rs) 1.88 이상이 필요합니다.

```bash
cargo install flot-cli
```

크레이트 이름은 `flot-cli`이고, 설치되는 명령은 `flot`입니다. Linux(x86_64, aarch64),
macOS(x86_64, arm64), Windows(x86_64, zip)용 실행 파일은 [릴리스 페이지](https://github.com/tobilife/flot/releases/latest)에
있습니다. 릴리스가 아닌 최신 `main`을 쓰려면
`cargo install --git https://github.com/tobilife/flot`을 사용하세요.

### 에이전트에 연결 (MCP)

```bash
claude mcp add flot -- flot mcp     # Claude Code
codex mcp add flot -- flot mcp      # Codex CLI
```

Claude Code에서는 대신 플러그인을 설치할 수 있습니다. 같은 서버를 등록하고, Claude가
변경이 동작한다고 말하기 전에 플로우를 실행하게 하는 스킬과, 플로우 파일, 증명이 읽는
파일, CI 워크플로, git 훅, Claude Code 자신의 설정 편집을 거부하는 훅을 더합니다. `flot`은 `PATH`에 있어야 합니다.

```text
/plugin install flot --marketplace tobilife/flot
```

실제 실행에서 flot을 언급하지 않은 프롬프트 "The slug tests fail. Fix slug.py and tell me
when the work is done."만으로, 플러그인을 설치한 Claude Code 2.1.330이 스킬을 불러와
`flot_run`을 호출하고 `passed`를 보고했으며, 비용은 $0.24였습니다. 증명을 `true`로
바꾸라는 요청에는 훅이 편집을 거부했고, Claude는 그 사실을 그대로 전했습니다($0.23).

`flot.toml`이 있는 프로젝트에서 에이전트를 다시 시작하세요(`flot init`이 시작용
파일을 만들어 줍니다). 그리고 이렇게 요청해 보세요.

```text
실패하는 테스트를 고쳐줘. 끝나면 flot_run을 실행해서 결과(verdict)와
검증되지 않은 주장이 있으면 보여줘.
```

에이전트는 실행을 계획(plan), 실행(run), 조회(show), 검증(verify), 비교(diff), 검색(search)하는
도구를 얻습니다. 주장이 증명되지 않은 실행은 도구 오류가 아니라 `unverified`로
돌아오므로, 에이전트가 증명 누락을 일시적인 오류로 착각할 수 없습니다. `flot_run`을
제외한 모든 도구는 읽기 전용으로 표시되어 있어서, 사람이 승인할 수 없는 헤드리스
에이전트는 `flot_run`만 미리 승인해 두면 됩니다([승인](../mcp.md#approvals) 참고).
모든 도구와 그 계약은 [MCP 서버](../mcp.md) 문서에 있습니다.

### 터미널에서 사용 (CLI)

```bash
flot init        # 시작용 flot.toml 작성
flot plan        # 무엇이 실행되고, 무엇이 재사용되며, 어떤 주장에 증명이 없는지
flot run         # 실행하고 영수증 기록
flot show        # 최근 실행의 단계, 주장, 한계(limitation)
```

이 저장소는 flot으로 스스로를 검증합니다([`flot.toml`](../../flot.toml)).

```
$ flot run --force
ran      format               ok        183ms  claims 1v/0u/0r  receipt 7c8c480702e0
ran      build                ok        876ms  claims 1v/0u/0r  receipt 2f5273d0f500
ran      lint                 ok        816ms  claims 1v/0u/0r  receipt 48a64dd4723f
ran      test                 ok        5657ms  claims 2v/0u/0r  receipt 3a0fd0619f68
    complete output: flot log test 1795521673839-85aac97d67a6

PASSED 1795521673839-85aac97d67a6  root 85aac97d67a6  16144ms
limitations:
  - test: inline output trimmed to 2048 bytes per stream; complete streams are in the store

$ flot run     # 바뀐 것이 없음: 영수증이 재사용되고 루트 다이제스트도 같음
cached   format               ok        183ms  claims 1v/0u/0r  receipt 7c8c480702e0
cached   build                ok        876ms  claims 1v/0u/0r  receipt 2f5273d0f500
cached   lint                 ok        816ms  claims 1v/0u/0r  receipt 48a64dd4723f
cached   test                 ok        5657ms  claims 2v/0u/0r  receipt 3a0fd0619f68

PASSED 1795521691818-85aac97d67a6  root 85aac97d67a6  412ms
```

flot 0.2.2로 기록한 출력입니다. 두 번째 실행은 영수증 네 개를 모두 재사용해 16초가 아니라
0.41초 걸렸고, 재사용된 단계는 기록될 때의 소요 시간만 보여 주고 다른 출력은 없습니다.
실행된 단계 아래에는 출력의 앞과 끝 몇 줄도 찍히는데 여기서는 생략했고, 전체는
`flot log test`로 볼 수 있습니다.

`1v/0u/0r`은 검증됨(verified), 미검증(unverified), 반박됨(refuted) 주장의 개수입니다.
모든 명령, 실행 지정 방법, `--json`, 종료 코드는 [CLI 레퍼런스](../cli.md)에 있습니다.

## flot의 동작 방식

```mermaid
flowchart LR
    you["사용자 또는 에이전트"] -->|flot run / flot_run| flot
    flot -->|실행| steps["단계<br/>셸 · 테스트 · 에이전트 CLI"]
    flot -->|이어서 각 주장을 증명| proofs["verify 명령"]
    flot -->|기록| store[("영수증<br/>.flot/")]
    store -->|flot bundle --sign| reviewer["다른 호스트의 리뷰어<br/>flot import · flot verify"]
```

단계는 무엇을 실행하고, 무엇을 읽고, 무엇을 주장하며, 그것을 어떻게 증명하는지
선언합니다.

```toml
[flow]
name = "release-candidate"
budget = { timeout_secs = 900 }

[[step]]
id = "implement"
run = "claude -p 'implement .flot-work/spec.md'"
needs = ["spec"]
inputs = [".flot-work/spec.md"]
outputs = ["src/**/*.rs"]
claims = ["the workspace still compiles after the agent's edits"]
verify = "cargo build --all-targets --quiet"
```

flot은 명령, 모든 입력 파일의 내용, 증명, 상위 단계의 영수증으로 **단계 키(step
key)**를 만듭니다. 키가 같으면 이전 영수증을 재사용하고 그 출력 파일을 복원합니다.
다르면 단계를 실행하고, 이어서 증명을 실행한 뒤, 새 영수증을 봉인합니다. 영수증 id는
일어난 일의 다이제스트이므로, 수정된 영수증은 더 이상 인증되지 않습니다. 실행 결과는
`failed`, `unverified`, `passed` 중 하나이며 이 순서로 우선합니다.

키, 영수증, 주장, 예산, 동시 실행, 출력, 보호된 증명, 변경 범위, 샌드박스 등 모든 필드는
[플로우 작성](../flows.md) 문서에 있습니다.

## flot이 보장하는 것

- **증명 없이는 통과 없음.** 주장은 단계가 성공한 뒤 증명 명령이 실행되어 0으로
  종료했을 때만 `verified`입니다. 실패, 시간 초과, 강제 종료, 실패 허용된 단계는
  아무것도 증명하지 않으며, 실행 중에 판단 대상 파일이 바뀐 증명도 인정되지 않습니다.
- **에이전트가 고칠 수 없는 증명.** `proofs_from = "origin/main"`이면 그 리비전과
  다른 증명(명령, 주장, 증명이 읽는 파일)의 주장은 unverified로 남고,
  `flot verify --proofs-from`은 기록된 실행을 같은 리비전에 비춰 봅니다. Claude Code
  플러그인은 그 파일들의 편집을 일어나기 전에 거부하고, 그것들이 검사되는지를 정하는
  CI 워크플로, git 훅, Claude Code 설정의 편집도 거부합니다.
- **범위 안의 변경.** `may_change = ["src/**"]`를 둔 단계가 git이 무시하지 않는 다른
  파일을 만들거나 고치거나 지우면 실패합니다. Linux에서는 `sandbox`로 그런 쓰기와
  네트워크를 커널이 거부하게 할 수 있습니다.
- **검증 가능한 영수증.** 모든 영수증은 다시 해시하면 자기 id가 되고, 실행의 결과와
  루트 다이제스트는 저장된 값을 읽지 않고 영수증에서 다시 계산됩니다. `flot verify`는
  저장된 모든 바이트를 다시 읽습니다.
- **믿을 수 있는 재사용.** 캐시 적중에는 같은 입력 내용과 같은 상위 영수증 위의 같은
  명령, 그리고 이 호스트의 봉인이 필요합니다. 실패는 절대 캐시되지 않습니다.
- **옮겨 다니는 증거.** `flot run --sign`은 실행을 기록하면서 바로 서명하고,
  `flot bundle`은 리뷰어가 가져와 검증할 수 있는 파일 하나를 만듭니다. 가져온 영수증은 리뷰어가 신뢰하는 서명자가 서명했을 때만, 그
  신뢰가 유지되는 동안만 로컬 작업을 대신합니다. 키를 바꿀 때는 `flot key rotate`가
  새 키와 옛 키가 함께 서명한 인계 문서를 만들고, 읽는 쪽은 `flot trust rotate`로 따라갑니다.
- **제한된 실행.** 시간 제한과 출력 제한은 작업을 멈추고 그 사실을 알립니다. 비용
  상한은 에이전트가 보고한 지출이 그 금액에 이르면 새 단계를 시작하지 않습니다.
- **하나의 계약.** CLI와 MCP 서버는 같은 함수를 호출하므로, 사람과 에이전트가 같은
  영수증, 결과, 다이제스트를 봅니다.

서명, 신뢰 목록, `.flot` 저장소는 [증거, 번들, 신뢰](../evidence.md) 문서에 있습니다.

## 예제 플로우

| 플로우 | 보여주는 것 |
|---|---|
| [`examples/slugify`](../../examples/slugify/flot.toml) | 에이전트 하나가 명세를 구현하고, 어떤 에이전트도 작성하지 않은 검사가 그것을 증명하며, 두 번째 에이전트가 리뷰합니다. Claude Code로 처음부터 끝까지 실행했고, [전체 이야기](../agents.md#a-real-run-end-to-end)에는 이 과정에서 flot 자체에서 찾은 버그도 있습니다. 에이전트 단계를 격리한 뒤 새로 실행하면 $0.48이 듭니다. |
| [`examples/slugify/compare.toml`](../../examples/slugify/compare.toml) | Claude Code와 Codex가 같은 명세를 두 후보로 구현합니다. 같은 검사가 각각을 증명하고, 통과한 것 중 가장 싼 것이 남습니다. [실제 실행 한 번](../agents.md#two-agents-on-the-same-spec)과 그 과정에서 찾은 버그 두 개. |
| [`examples/agent-review.toml`](../../examples/agent-review.toml) | 명세, 구현, 리뷰를 세 개의 에이전트 단계로 나누고, 프로젝트의 테스트가 구현을 증명합니다. |
| [`examples/parallel-review.toml`](../../examples/parallel-review.toml) | 현재 diff를 독립된 에이전트 리뷰어 세 명(보안, 성능, 계약)이 동시에 검토한 뒤 테스트 게이트를 통과합니다. [실제 실행 한 번](../agents.md#three-reviewers-in-parallel)에서 문서화되지 않은 기본값을 찾아냈습니다. 에이전트 단계를 격리하자 한 번 실행 비용이 $3.25에서 $0.42로 줄었습니다. |
| [`flot.toml`](../../flot.toml) | 이 저장소 자체의 게이트: 포맷, 빌드, 린트, 테스트를 각각 증명과 함께 실행합니다. |

예제는 실행하기 전에 프로젝트 루트로 복사하세요. 경로와 `.flot` 저장소는 플로우
파일 기준입니다. 에이전트 단계와 비용 집계는 [코딩 에이전트 실행](../agents.md)
문서에 있습니다.

## FAQ

<details>
<summary><strong>flot은 Claude Code나 Codex 플러그인인가요?</strong></summary>

아닙니다. flot은 독립된 바이너리입니다. 에이전트와 양방향으로 함께 동작합니다.
에이전트가 MCP로 flot을 호출하거나(`flot mcp`), flot이 단계 안에서 에이전트를
호출합니다(`claude -p …`, `codex exec …`). 훅, 프롬프트, 에이전트 정의를 설치하지
않으며, 에이전트 바깥에서 작업을 판정합니다.

</details>

<details>
<summary><strong>어떤 에이전트에서 쓸 수 있나요?</strong></summary>

stdio 방식의 로컬 MCP 서버를 지원하는 모든 에이전트, 그리고 단계 안에서 쓸 수 있는
CLI가 있는 모든 에이전트입니다. Claude Code와 Codex 등록 방법은 위에 있고, 프로토콜
세부 사항은 [MCP 서버](../mcp.md) 문서에 있습니다.

</details>

<details>
<summary><strong>flot이 실행하는 단계를 샌드박스에 가두나요?</strong></summary>

기본으로는 아닙니다. 단계는 사용자의 권한과 환경으로 실행됩니다.
`sandbox = { write = ["src"] }`를 둔 단계는 Linux에서 네트워크 없이, `src` 아래에만 쓰도록
커널이 강제합니다(네트워크 네임스페이스와 Landlock). 이를 강제할 수 없는 곳에서는 단계를
실행하지 않습니다. 읽기는 제한하지 않으므로 컨테이너는 아닙니다. 직접 읽어 본 플로우만
실행하고, 에이전트에게는 원래 줄 수 있는 만큼의 권한만 주세요.

</details>

<details>
<summary><strong>실행 결과가 왜 <code>unverified</code>인가요?</strong></summary>

적어도 하나의 주장이 증명으로 결론나지 않았다는 뜻입니다. 단계에 `verify` 명령이
없거나, 단계가 실패했거나 `allow_failure`로 허용됐거나, 증명이 시간 초과·강제
종료되었거나 판단 대상 파일이 바뀐 경우입니다. `flot show`는 검증되지 않은 주장을
하나씩 보여 주고, `flot plan`은 실행 전에 증명이 없는 주장을 알려 줍니다.

</details>

<details>
<summary><strong>flot이 내 코드를 어딘가로 보내나요?</strong></summary>

아닙니다. flot은 로컬에서 실행되며 네트워크 코드가 없습니다. `flot push`와
`flot pull`은 실행을 공유하려고 `git`을 실행하며, 여러분이 지정한 원격에 여러분이 실행할
때만 연결합니다. 공유되는 실행에는 단계의 출력 전체가 들어 있으므로, `flot bundle`과
`flot push`는 자격 증명처럼 보이는 것(GitHub·클라우드·모델 제공자 토큰, 개인 키)이 든
실행을 거부하고 그것이 든 단계를 알려 줍니다. 단계로 실행하는 에이전트는 각자의 제공자와 각자의 데이터 정책에 따라
통신합니다.

</details>

<details>
<summary><strong>Windows에서 동작하나요?</strong></summary>

네. [Git for Windows](https://gitforwindows.org)가 설치되어 있어야 합니다. 플로우는 모든
플랫폼에서 `sh` 기준으로 작성하며, flot은 Git for Windows에 들어 있는 `sh`로 단계를 실행합니다.
단계와 그 단계가 시작한 모든 프로세스는 Windows 작업 개체(job object)에 묶이므로, 시간 초과나
Ctrl-C가 전부를 멈춥니다. 테스트 스위트를 Windows에서도 실행합니다. 설치는
`cargo install flot-cli`로 합니다. Windows에는 시그널과 실행 권한 비트가 없어서, 강제 종료된
증명은 받은 종료 코드로 판정되고, Linux나 macOS에서 실행 권한이 있는 파일은 키가 달라져
그런 영수증은 두 환경 사이에서 재사용되지 않습니다.

</details>

<details>
<summary><strong>버그를 만나면 어떻게 하나요?</strong></summary>

`cargo install flot-cli`로 업데이트한 뒤 다시 시도하세요. 그래도 재현되면 `flot --version`, 플로우 파일, 실행한 명령과 출력을 담아
[이슈를 열어 주세요](https://github.com/tobilife/flot/issues).

</details>

## 문서

문서는 영어로 작성되어 있습니다.

- [플로우 작성](../flows.md): 단계, 키, 영수증, 주장, 재시도(attempts), 후보 비교(candidates), 예산, 동시 실행, 출력, 보호된 증명, 변경 범위, 샌드박스.
- [CI에서](../ci.md): GitHub 액션, 풀 리퀘스트 보고서, 개발자가 서명한 실행의 재사용.
- [CLI 레퍼런스](../cli.md): 명령, 실행과 플로우 지정, `--json`, 종료 코드.
- [MCP 서버](../mcp.md): 등록, 도구, 응답, 오류, 취소.
- [코딩 에이전트 실행](../agents.md): 에이전트 실행 전체 사례, 실패 내용을 받아 다시 시도하기, 두 에이전트 비교, 비용과 비용 상한.
- [증거, 번들, 신뢰](../evidence.md): 번들, git 원격으로 공유하기, 서명, 키 교체, 신뢰 목록, `.flot` 저장소.
- [아키텍처](../architecture.md): 계층, 강제되는 의존성 규칙, 현재 범위.

취약점은 [SECURITY.md](../../SECURITY.md)를 통해 제보해 주세요.

## 기여하기

버그 제보나 기능 제안은 [이슈](https://github.com/tobilife/flot/issues)로, 코드나 문서
개선은 [풀 리퀘스트](https://github.com/tobilife/flot/pulls)로 보내 주세요. 개발 환경과
검사 방법은 [CONTRIBUTING.md](../../CONTRIBUTING.md)에 있습니다.

## 라이선스

[MIT](../../LICENSE)
