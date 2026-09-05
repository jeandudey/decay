//! The CPU architectures `has_function` answers for: the intersection of
//! buck2's `prelude//cpu/constraints:cpu` values and zig's `linux` targets
//! that decay's own `linux`-system gating can actually reach.
//!
//! Two of buck2's seven `cpu` values drop out of that intersection entirely:
//! `arm64_32` has no `linux` target at all in zig — it names an ILP32 ABI
//! Apple ships only for watchOS — and `wasm32`'s only zig `musl`-like target
//! is `wasm32-wasi-musl`, whose system is `wasi`, not `linux`.
//!
//! Shared, via `#[path]`, between `build.rs` (which only needs [`Cpu::ALL`]
//! and [`Cpu::zig_arch`]/[`Cpu::musl_target`] to generate musl's half of the
//! database) and `src/lib.rs` (which additionally needs [`Cpu::glibc_abi`]
//! and exposes [`Cpu::buck2_value`] for `decay`'s own oracle to build a
//! `select()` on).
#![allow(dead_code)] // not every consumer of this file uses every method.

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Cpu {
    X86_64,
    X86_32,
    Arm64,
    Arm32,
    Riscv64,
}

impl Cpu {
    pub const ALL: [Cpu; 5] = [
        Self::X86_64,
        Self::X86_32,
        Self::Arm64,
        Self::Arm32,
        Self::Riscv64,
    ];

    /// The `prelude//cpu/constraints:cpu` value buck2 uses for this arch.
    pub fn buck2_value(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::X86_32 => "x86_32",
            Self::Arm64 => "arm64",
            Self::Arm32 => "arm32",
            Self::Riscv64 => "riscv64",
        }
    }

    /// zig's (and glibc's abilist's) arch component of a `<arch>-linux-<abi>`
    /// triple.
    pub fn zig_arch(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::X86_32 => "x86",
            Self::Arm64 => "aarch64",
            Self::Arm32 => "arm",
            Self::Riscv64 => "riscv64",
        }
    }

    /// The abi component glibc's own abilist names this arch's gnu column
    /// with. Almost always plain `"gnu"` — except arm32, which glibc only
    /// ever ships as hard-float EABI, so its column is `"gnueabihf"`
    /// instead (and a bare `arm-linux-musl` target, while `zig cc -c`
    /// accepts it silently, is refused outright by `-Xclang -ast-dump`;
    /// `musl_target` below spells it out for the same reason).
    pub fn glibc_abi(self) -> &'static str {
        match self {
            Self::Arm32 => "gnueabihf",
            _ => "gnu",
        }
    }

    /// The full `-target` triple `zig cc` builds musl for this arch with.
    pub fn musl_target(self) -> String {
        let abi = match self {
            Self::Arm32 => "musleabihf",
            _ => "musl",
        };
        format!("{}-linux-{abi}", self.zig_arch())
    }
}
