# ADR-0002: 배포 target `aarch64-unknown-linux-gnu`, 빌드는 Docker linux/aarch64 컨테이너

- **상태**: 제안됨 → S0 슬라이스에서 실제 빌드·Pi 실행으로 확인한 뒤 "채택됨"으로 바꾼다
- **날짜**: 2026-10-15

## 맥락
2026-10-15 실측: 개발 머신은 macOS Apple Silicon(Docker Desktop daemon `linux/aarch64` 네이티브), 운영 Pi는 Debian 13 trixie aarch64, glibc 2.41. Mac에는 zig·cross·크로스 gcc가 없다. bundled SQLite와 aws-lc-rs는 C/C++ 컴파일이 필요하다. Pi는 build host로 쓰지 않는다(microSD·기존 서비스).

## 결정
- target은 `aarch64-unknown-linux-gnu`. 바이너리는 glibc 2.41 이하 symbol version에만 링크되어야 한다.
- 배포 바이너리는 `docker run --platform linux/aarch64` 로 Docker 공식 `rust:1-trixie`(또는 `1-slim-trixie` + 필요한 build-essential) 컨테이너 안에서 `cargo build --release`로 만든다. 스크립트 `deploy/build-aarch64.sh`가 유일한 진입점이며 결과는 `target/aarch64/aarch64-unknown-linux-gnu/release/platformd`.
- cargo registry·target 캐시는 named volume으로 유지해 재빌드 시간을 줄인다.

## 대안
- **cargo-zigbuild (`--target aarch64-unknown-linux-gnu.2.41`)** — 보류: 설치가 필요하고 aws-lc-rs의 C/C++ 빌드가 zig cc에서 되는지 별도 확인 필요. 컨테이너 경로가 실패하면 1순위 대안.
- **musl static** — 기각(기본값으로): 운영 OS ABI와 다른 libc 동작(DNS, 할당자)을 따로 검증해야 한다.
- **Pi에서 직접 빌드** — 기각: TECH 문서 원칙.

## 근거
컨테이너가 target과 같은 architecture라 에뮬레이션·sysroot 없이 네이티브 C 툴체인이 그대로 쓰인다. trixie 이미지는 운영 OS와 같은 계열이라 glibc 상한을 자연히 지킨다. Docker Hub manifest에 두 태그의 `linux/arm64`가 있음을 2026-10-15 확인했다(이미지 내부 glibc 버전과 빌드 성공은 S0에서 기록).

## 결과 · 트레이드오프
- 좋아지는 것: 호스트 오염 없음, 재현 가능한 빌드 환경, 운영 OS와 같은 libc 계열.
- 감수하는 것: Docker Desktop 필수, 바인드 마운트 I/O로 첫 빌드가 느림, Mac 네이티브 `cargo test`와 배포 빌드가 다른 target(두 경로를 구분해 기록).
- 영향받는 인터페이스: `IF-OPS-001`(`--version` git sha 주입), 배포 스크립트.

## 재평가 트리거
컨테이너 빌드가 Pi에서 glibc symbol 오류를 내거나 빌드 시간이 개발 흐름을 막으면 cargo-zigbuild로 전환을 검토한다. 운영 Pi가 trixie 이후 버전으로 올라가면 이미지 태그를 맞춘다.
