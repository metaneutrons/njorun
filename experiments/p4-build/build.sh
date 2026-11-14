#!/usr/bin/env bash
set -euo pipefail

EXPERIMENT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="$(cd "$EXPERIMENT_DIR/../.." && pwd -P)"
BUILD_ROOT="$REPO_ROOT/target/build/p4-build"
IDF_DIR="$BUILD_ROOT/esp-idf"
RUST_TOOLCHAIN="nightly-2026-09-18"
IDF_COMMIT="fff9895c82d744c7237be8847347bdd1b07c6643"
ESP_IDF_SHARED_TOOLS="${ESP_IDF_SHARED_TOOLS:-$HOME/.espressif/tools}"
TARGET="riscv32imafc-esp-espidf"
HOST_OS="$(uname -s)"

case "$HOST_OS" in
    Darwin)
        HOST_BUILD_DIR="$BUILD_ROOT"
        PYTHON3="${PYTHON3:-python3.14}"
        EXPECTED_PYTHON_MAJOR_MINOR="3.14"
        EXPECTED_PYTHON_VERSION="3.14.10"
        EXPECTED_CLANG_VERSION="20.1.1"
        ESP_CLANG_VERSION="esp-20.1.1_20250829"
        ;;
    Linux)
        HOST_BUILD_DIR="$BUILD_ROOT/linux"
        PYTHON3="${PYTHON3:-python3.12}"
        EXPECTED_PYTHON_MAJOR_MINOR="3.12"
        EXPECTED_PYTHON_VERSION="Ubuntu 24.04 Python 3.12.x"
        EXPECTED_CLANG_VERSION="18.1.3"
        ;;
    *)
        echo "Unsupported build host: $HOST_OS (expected Darwin or Linux)." >&2
        exit 1
        ;;
esac

IDF_TOOLS_PATH="$HOST_BUILD_DIR/esp-idf-tools"
RUSTUP_HOME="$HOST_BUILD_DIR/rustup-home"
CARGO_HOME="$HOST_BUILD_DIR/cargo-home"

mkdir -p "$HOST_BUILD_DIR" "$CARGO_HOME"
export CARGO_HOME

if [[ "$HOST_OS" == "Linux" ]]; then
    export RUSTUP_HOME
    mkdir -p "$RUSTUP_HOME" "$CARGO_HOME"
    export PATH="$CARGO_HOME/bin:$PATH"
    if ! command -v rustup >/dev/null || ! command -v cargo >/dev/null; then
        echo "Linux builds require rustup and cargo on PATH; their toolchain/cache will be kept under $HOST_BUILD_DIR." >&2
        exit 1
    fi
fi

if [[ $# -gt 1 || ( $# -eq 1 && "$1" != "--clean" ) ]]; then
    echo "Usage: $0 [--clean]" >&2
    exit 2
fi

if [[ "${1:-}" == "--clean" ]]; then
    "$PYTHON3" - "$HOST_BUILD_DIR" <<'PY'
import shutil
import sys
from pathlib import Path

build_dir = Path(sys.argv[1]).resolve()
for name in ("cargo", "sdkconfig"):
    path = build_dir / name
    if path.is_dir() and not path.is_symlink():
        shutil.rmtree(path)
    elif path.exists() or path.is_symlink():
        path.unlink()
PY
fi

if [[ ! -d "$IDF_DIR/.git" ]]; then
    if [[ -e "$IDF_DIR" ]]; then
        echo "Refusing to use $IDF_DIR: expected an ESP-IDF git checkout." >&2
        exit 1
    fi
    git clone --depth 1 --branch v6.1 --recurse-submodules --shallow-submodules \
        https://github.com/espressif/esp-idf.git "$IDF_DIR"
else
    current_commit="$(git -C "$IDF_DIR" rev-parse HEAD)"
    if [[ "$current_commit" != "$IDF_COMMIT" ]]; then
        git -C "$IDF_DIR" fetch --depth 1 origin refs/tags/v6.1
        git -C "$IDF_DIR" checkout --force --detach "$IDF_COMMIT"
    fi
    git -C "$IDF_DIR" submodule update --init --recursive --depth 1
fi

actual_commit="$(git -C "$IDF_DIR" rev-parse HEAD)"
if [[ "$actual_commit" != "$IDF_COMMIT" ]]; then
    echo "ESP-IDF commit mismatch: expected $IDF_COMMIT, got $actual_commit." >&2
    exit 1
fi

link_tool_version() {
    local tool="$1"
    local version="$2"
    local source="$ESP_IDF_SHARED_TOOLS/$tool/$version"
    local destination="$IDF_TOOLS_PATH/$tool/$version"

    if [[ ! -d "$source" ]]; then
        echo "Missing preinstalled ESP-IDF tool: $source" >&2
        echo "Install ESP-IDF v6.1 tools with Espressif EIM, then rerun this script." >&2
        exit 1
    fi

    mkdir -p "$(dirname "$destination")"
    if [[ ! -e "$destination" && ! -L "$destination" ]]; then
        ln -s "$source" "$destination"
    elif [[ ! -d "$destination" ]]; then
        echo "Tool path exists but is not a directory: $destination" >&2
        exit 1
    fi
}

PYTHON_VERSION="$($PYTHON3 --version 2>&1 | awk '{print $2}')"
PYTHON_MAJOR_MINOR="$($PYTHON3 -c 'import sys; print(f"{sys.version_info.major}.{sys.version_info.minor}")')"
if [[ "$PYTHON_MAJOR_MINOR" != "$EXPECTED_PYTHON_MAJOR_MINOR" ]]; then
    echo "Expected Python $EXPECTED_PYTHON_MAJOR_MINOR.x, found $PYTHON_VERSION via $PYTHON3." >&2
    exit 1
fi
if [[ "$HOST_OS" == "Darwin" && "$PYTHON_VERSION" != "$EXPECTED_PYTHON_VERSION" ]]; then
    echo "Expected Python $EXPECTED_PYTHON_VERSION, found $PYTHON_VERSION via $PYTHON3." >&2
    exit 1
fi

export IDF_PATH="$IDF_DIR"
export IDF_TOOLS_PATH
export ESP_IDF_VERSION=6.1.0
export MCU=esp32p4
export ESP_IDF_TOOLS_INSTALL_DIR=fromenv
export ESP_IDF_SDKCONFIG="$HOST_BUILD_DIR/sdkconfig"
export IDF_PYTHON_CHECK_CONSTRAINTS=1
export PIP_CACHE_DIR="$HOST_BUILD_DIR/pip-cache"
export CARGO_WORKSPACE_DIR="$EXPERIMENT_DIR"
export CARGO_TARGET_DIR="$HOST_BUILD_DIR/cargo"
export CARGO_INCREMENTAL=0
export IDF_PYTHON_ENV_PATH="$IDF_TOOLS_PATH/python_env/idf6.1_py${EXPECTED_PYTHON_MAJOR_MINOR}_env"

if [[ "$HOST_OS" == "Darwin" ]]; then
    IDF_TOOL_ROOT="$IDF_TOOLS_PATH"
else
    IDF_TOOL_ROOT="$IDF_TOOLS_PATH/tools"
fi

GCC_BIN="$IDF_TOOL_ROOT/riscv32-esp-elf/esp-15.2.0_20251204/riscv32-esp-elf/bin"
CMAKE_BIN="$IDF_TOOL_ROOT/cmake/4.0.3/bin"
NINJA_BIN="$IDF_TOOL_ROOT/ninja/1.12.1"
if [[ "$HOST_OS" == "Darwin" ]]; then
    CMAKE_BIN="$IDF_TOOL_ROOT/cmake/4.0.3/CMake.app/Contents/bin"
    CLANG_BIN="$IDF_TOOL_ROOT/esp-clang/$ESP_CLANG_VERSION/esp-clang/bin"
    LIBCLANG_DIR="$IDF_TOOL_ROOT/esp-clang-libs/$ESP_CLANG_VERSION/esp-clang/lib"
    CLANG_EXECUTABLE="$CLANG_BIN/clang"
    link_tool_version riscv32-esp-elf esp-15.2.0_20251204
    link_tool_version cmake 4.0.3
    link_tool_version ninja 1.12.1
    link_tool_version esp-rom-elfs 20241011
    link_tool_version esp-clang "$ESP_CLANG_VERSION"
    link_tool_version esp-clang-libs "$ESP_CLANG_VERSION"
else
    # idf_tools.py reads tools.json from the pinned ESP-IDF commit and verifies
    # each downloaded archive against its SHA-256 before extracting it.
    "$PYTHON3" "$IDF_PATH/tools/idf_tools.py" --idf-path "$IDF_PATH" --non-interactive install \
        --targets esp32p4 \
        riscv32-esp-elf@esp-15.2.0_20251204 \
        cmake@4.0.3 \
        ninja@1.12.1 \
        esp-rom-elfs@20241011
    CLANG_BIN="/usr/bin"
    CLANG_EXECUTABLE="$CLANG_BIN/clang-18"
    LIBCLANG_DIR="/usr/lib/llvm-18/lib"
    if [[ ! -e "$LIBCLANG_DIR/libclang.so" ]]; then
        echo "Missing Ubuntu libclang 18 development files; install libclang-18-dev=1:18.1.3-1ubuntu1." >&2
        exit 1
    fi
fi
export ESP_ROM_ELF_DIR="$IDF_TOOL_ROOT/esp-rom-elfs/20241011"
export LIBCLANG_PATH="$LIBCLANG_DIR"
export CLANG_PATH="$CLANG_EXECUTABLE"

if [[ ! -x "$IDF_PYTHON_ENV_PATH/bin/python" ]]; then
    "$PYTHON3" "$IDF_PATH/tools/idf_tools.py" install-python-env
fi

CONSTRAINTS="$IDF_TOOLS_PATH/espidf.constraints.v6.1.txt"
EXPECTED_CONSTRAINTS_SHA256="192e8c82a7890164a4e8fe6fafe8423a47f6042c84b5f6a500a003640ae4d32b"
if [[ ! -f "$CONSTRAINTS" ]]; then
    echo "Missing ESP-IDF 6.1 Python constraints file: $CONSTRAINTS" >&2
    exit 1
fi
"$PYTHON3" - "$EXPECTED_CONSTRAINTS_SHA256" "$CONSTRAINTS" <<'PY'
import hashlib
import sys
from pathlib import Path

expected, file_path = sys.argv[1:]
actual = hashlib.sha256(Path(file_path).read_bytes()).hexdigest()
if actual != expected:
    raise SystemExit(f"ESP-IDF constraints SHA-256 mismatch: expected {expected}, got {actual}")
print(f"ESP-IDF constraints SHA-256 OK: {actual}")
PY

export PATH="$HOST_BUILD_DIR/tooling/bin:$GCC_BIN:$CMAKE_BIN:$NINJA_BIN:$IDF_PYTHON_ENV_PATH/bin:$CLANG_BIN:$ESP_ROM_ELF_DIR:$IDF_PATH/tools:$PATH"

# esp-idf-sys resolves `python` from PATH for CMake. Ubuntu also has a
# system Python in the same directory as clang-18.
if [[ "$(command -v python)" != "$IDF_PYTHON_ENV_PATH/bin/python" ]]; then
    echo "The ESP-IDF Python environment must supply CMake's interpreter." >&2
    exit 1
fi
python -c 'import idf_component_manager.prepare_components'

if ! riscv32-esp-elf-gcc --version | grep -Fq 'esp-15.2.0_20251204'; then
    echo "Unexpected RISC-V compiler version." >&2
    exit 1
fi
if ! cmake --version | grep -Fq '4.0.3'; then
    echo "Unexpected CMake version." >&2
    exit 1
fi
if [[ "$(ninja --version)" != "1.12.1" ]]; then
    echo "Unexpected Ninja version." >&2
    exit 1
fi
if ! "$CLANG_PATH" --version | grep -Fq "$EXPECTED_CLANG_VERSION"; then
    echo "Unexpected host Clang version for bindgen." >&2
    exit 1
fi

rustup toolchain install "$RUST_TOOLCHAIN" --profile minimal --component rust-src

if [[ ! -x "$HOST_BUILD_DIR/tooling/bin/ldproxy" ]]; then
    CARGO_TARGET_DIR="$HOST_BUILD_DIR/ldproxy-target" cargo +"$RUST_TOOLCHAIN" install \
        --locked --version 0.3.5 --root "$HOST_BUILD_DIR/tooling" ldproxy
fi
export CARGO_TARGET_DIR="$HOST_BUILD_DIR/cargo"

actual_idf_version="$("$IDF_PYTHON_ENV_PATH/bin/python" "$IDF_PATH/tools/idf.py" --version)"
if [[ "$actual_idf_version" != "ESP-IDF v6.1" ]]; then
    echo "Unexpected ESP-IDF version: $actual_idf_version" >&2
    exit 1
fi

printf 'Building %s for %s with Rust %s and ESP-IDF %s (%s)\n' \
    "$(basename "$EXPERIMENT_DIR")" "$TARGET" "$RUST_TOOLCHAIN" "$actual_idf_version" "$actual_commit"

cd "$EXPERIMENT_DIR"
cargo +"$RUST_TOOLCHAIN" fetch --locked

cargo +"$RUST_TOOLCHAIN" build --locked --release -Zbuild-std=std,panic_abort --target "$TARGET"
