//
//
// ccrush: The fastest C compiler in the world.
//
// repository: https://github.com/rakivo/ccrush.
//
//
//
// Yes, this is a single file containing the ENTIRETY of the compiler.
//
// I've had multiple reasons to keep it that way, but the main ones are:
//
//   1. Locality of reference. Meaning that people won't have to jump around
//        to read the source code. FUCK CLEAN CODE.
//
//
//   2. To flex on all the clean code paradigm users.
//        I'm a huge proponent of simple code — code that is Semantically
//        Compressed, with no bullshit and unnecessary abstraction.
//        What's "Semantic Compression"? Read what Casey Muratori has to say
//        about it:
//
//        https://caseymuratori.com/blog_0015
//
//
//        Semantic Compression — Casey Muratori
//
//        I look at programming as having essentially two parts: figuring out
//        what the processor actually needs to do to get something done, and
//        then figuring out the most efficient way to express that in the
//        language I'm using. Increasingly, it is the latter that accounts
//        for what programmers actually spend their time on: wrangling all
//        those algorithms and all that math into a coherent whole that
//        doesn't collapse under its own weight.
//
//        So any experienced programmer who's any good has had to come up
//        with some way — if even just by intuition — of thinking about what
//        it means to program efficiently. By "efficiently", I don't just mean
//        that the code is optimized. Rather, I mean that the development of
//        the code is optimized — that the code is structured in such a way
//        as to minimize the amount of human effort necessary to type it, get
//        it working, modify it, and debug it enough for it to be shippable.
//
//        I like to think of efficiency as holistically as possible. If you
//        look at the development process for a piece of code as a whole, you
//        won't overlook any hidden costs. Given a certain level of
//        performance and quality required by the places the code gets used,
//        beginning at its inception and ending with the last time the code
//        is ever used by anyone for any reason, the goal is to minimize the
//        total amount of human effort it costs. This includes the time to
//        type it in. It includes the time to debug it. It includes the time
//        to modify it. It includes the time to adapt it for other uses. It
//        includes any work done to other code to get it to work with this
//        code that perhaps wouldn't have been necessary if the code were
//        written differently. All work on the code for its entire usable
//        lifetime is included.
//
//

// TODO(#2): Structs
// TODO(#21): Struct literals
// TODO(#23): String literal merging: "foo""bar"
// TODO(#20): Enum repr type C23 extension.
// TODO(#4): Enums
// TODO(#3): Function pointers
// TODO(#7): __attribute__
// TODO(#9): Global extern symbols
// TODO(#10): Stringify, __VA_ARGS__ and concat in macros
// TODO(#12): {} Initializers
// TODO(#13): Debug info
// TODO(#15): Stack unwind info
// TODO(#14): GCC statement expressions
// TODO(#16): More declarators support: `void (*(*fp)(int))(float)`
// TODO(#17): __builtin_bswap16, __builtin_*...
// TODO(#24): Proper implicit coercion: int <-> float, etc

// TODO(#18): Resolve the situation with sret and VLA hidden slot using the same slot...?

//
// @Cleanup: There's lots of unnecessary std::process::exit calls.
//

#[cfg(feature = "mimalloc")]
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::io;
use std::fs::File;
use std::borrow::Cow;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::ops::{Deref, DerefMut};

use smallstr::SmallString;
use thiserror::Error;
use smallvec::{SmallVec, smallvec};
use nohash_hasher::IntMap;
use memmap2::{Mmap, MmapMut, MmapOptions};
use cranelift_entity::{PrimaryMap, SparseMap, SparseMapValue, entity_impl};

#[allow(non_camel_case_types, unused)]
pub use {
    bool as bx,   // x
    bool as b8,   // 1
    bool as b16,  // 2
    bool as b32,  // 4
    bool as b64,  // 8
    bool as b128, // 16
};

#[inline(always)]
const fn hash_ident(s: &str) -> u64 {
    let b = s.as_bytes();
    let mut h = 0xcbf29ce484222325u64;
    let mut i = 0;
    while i < b.len() {
        h = (h ^ b[i] as u64).wrapping_mul(0x100000001b3);
        i += 1;
    }
    h
}

type RodataHash = u64;

#[inline(always)]
fn hash_bytes_for_rodata_interning(bytes: &[u8]) -> RodataHash {
    wyhash::wyhash(bytes, 0)
}

#[inline]
const fn align(x: usize, a: usize) -> usize {
    (x + a - 1) & !(a - 1)
}

#[inline]
fn str_into_boxed_path(s: impl AsRef<str>) -> Box<Path> {
    let s: &str = s.as_ref();
    let p: &Path = s.as_ref();
    p.into()
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct FileRef(pub u16);
entity_impl!(FileRef, "FileRef", f, FileRef(f as _), f.0 as u32);

pub struct FileInfo {
    pub path: Box<str>,
    pub data: FileData,
}

pub enum FileData {
    Mapped(Mmap),
    Owned(Box<[u8]>),
}

impl AsRef<[u8]> for FileData {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        self.slice()
    }
}

impl FileData {
    #[inline]
    fn slice(&self) -> &[u8] {
        match self { FileData::Mapped(m) => &m, FileData::Owned(v) => v }
    }
}

pub struct SrcArena {
    pub files: PrimaryMap<FileRef, FileInfo>,
}

impl SrcArena {
    #[inline]
    pub fn new() -> Self {
        Self { files: PrimaryMap::with_capacity(64) }
    }

    // for real files - mmap'd, no copy
    #[inline]
    pub fn add_path(&mut self, path: &Path) -> io::Result<FileRef> {
        let file    = File::open(path)?;
        let mapping = unsafe { MmapOptions::new().map(&file)? };

        let id = self.files.push(FileInfo {
            path: path.to_string_lossy().into(),
            data: FileData::Mapped(mapping),
        });

        Ok(id)
    }

    // for tests / PP::from_bytes - owned vec, no mmap
    #[inline]
    pub fn add_bytes(&mut self, path: &Path, src: impl Into<Box<[u8]>>) -> FileRef {
        let src = src.into();

        self.files.push(FileInfo {
            path: path.to_string_lossy().into(),
            data: FileData::Owned(src),
        })
    }

    #[inline]
    pub fn slice(&self, fid: FileRef) -> &[u8] {
        self.files[fid].data.slice()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub file: FileRef,
    pub start: u32,
    pub len: u16
}

impl Span {
    pub const POISONED: Self = unsafe { core::mem::zeroed() };

    #[inline]
    pub fn s<'a>(&self, arena: &'a SrcArena) -> &'a str {
        let src = arena.slice(self.file);
        let s   = self.start as usize;
        unsafe { std::str::from_utf8_unchecked(&src[s..s + self.len as usize]) }
    }

    #[inline]
    pub fn merge(self, o: Span) -> Span {
        let end = (o.start + o.len as u32).max(self.start + self.len as u32);
        Span {
            file: self.file,
            start: self.start.min(o.start),
            len: (end - self.start.min(o.start)) as u16
        }
    }
}

#[inline]
pub fn emit_diag(msg: &str, span: Span, arena: &SrcArena) {
    emit_diag_impl(msg, span, arena, false)
}

#[inline]
pub fn emit_diag_warning(msg: &str, span: Span, arena: &SrcArena) {
    emit_diag_impl(msg, span, arena, true)
}

pub fn emit_diag_impl(msg: &str, span: Span, arena: &SrcArena, is_warning: bool) {
    const R: &str = "\x1b[1;31m"; const C: &str = "\x1b[36m";
    const B: &str = "\x1b[1m";    const X: &str = "\x1b[0m";
    const Y: &str = "\x1b[0;33m";

    let color = if is_warning { Y }         else { R };

    eprintln!(
        "{color}{what}{X}{B}: {msg}{X}",
        what  = if is_warning { "warning" } else { "error" }
    );
    if span == Span::POISONED { return; }

    let src    = arena.slice(span.file);
    let path   = &arena.files[span.file].path;
    let before = &src[..span.start as usize];

    let line = before.iter().filter(|&&b| b == b'\n').count() + 1;
    let col  = {
        let mut c = 0usize;
        for &b in before.iter().rev().take_while(|&&b| b != b'\n') {
            if b == b'\t' { c += 4 - (c % 4); } else { c += 1; }
        }
        c + 1
    };
    eprintln!("  {C}-->{X} {path}:{line}:{col}");

    let ls = before.iter().rposition(|&b| b == b'\n').map(|i| i+1).unwrap_or(0);
    let le = src[ls..].iter().position(|&b| b == b'\n').map(|i| ls+i).unwrap_or(src.len());

    //
    // Visual column of span start within the line
    //
    let hl_s = {
        let mut col = 0usize;
        for &b in &src[ls..span.start as usize] {
            if b == b'\t' { col += 4 - (col % 4); } else { col += 1; }
        }
        col
    };
    let hl_l = (span.len as usize).min(le.saturating_sub(span.start as usize)).max(1);

    //
    // Expand tabs for display
    //
    let line_txt = {
        let raw = &src[ls..le];
        let mut out = String::with_capacity(raw.len());
        let mut col = 0usize;
        for &b in raw {
            if b == b'\t' {
                let n = 4 - (col % 4);
                for _ in 0..n { out.push(' '); }
                col += n;
            } else {
                out.push(b as char);
                col += 1;
            }
        }
        out
    };
    let lnum     = format!("{line}");
    let pad      = " ".repeat(lnum.len());

    eprintln!("{pad} {C}|{X}");
    eprintln!("{B}{lnum}{X} {C}|{X} {line_txt}");
    eprintln!("{pad} {C}|{X} {}{color}{}{X}", " ".repeat(hl_s), "^".repeat(hl_l));
    eprintln!("{pad} {C}|{X}");
}

fn debug_tokens(path: &Path) {
    let mut pp = match PP::new(path, &std::env::args().collect::<Vec<_>>()) {
        Ok(pp) => pp,
        Err(e) => { eprintln!("{e}"); return; }
    };

    loop {
        let t = pp.current_token;
        if t.kind == TK::Eof { break; }
        let s = t.s(&pp.src_arena);
        eprintln!("{:?} {:?}", t.kind, s);
        pp.next();
    }
}

#[derive(Debug, Error)]
pub enum PPError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("#error: {msg}")]
    Error { span: Span, msg: String },

    #[error("file not found: '{path}'")]
    NotFound  { span: Span, path: String },

    #[error("unterminated macro call")]
    Unterminated { span: Span },

    #[error("argument count mismatch for '{name}' (expected {expected})")]
    ArgumentCountMismatch { span: Span, name: String, expected: usize },

    #[error("#include nested too deep")]
    IncludeDepth { span: Span },

    #[error("unknown directive '#{name}'")]
    BadDirective { span: Span, name: String },
}

impl PPError {
    #[inline]
    fn span(&self) -> Span {
        match self {
            PPError::Io(_)                     => Span::POISONED,
            PPError::Error       { span, .. }  => *span,
            PPError::NotFound    { span, .. }  => *span,
            PPError::Unterminated{ span, .. }  => *span,
            PPError::ArgumentCountMismatch { span, .. }  => *span,
            PPError::IncludeDepth{ span, .. }  => *span,
            PPError::BadDirective{ span, .. }  => *span,
        }
    }

    #[inline]
    pub fn emit(&self, arena: &SrcArena) {
        emit_diag(&self.to_string(), self.span(), arena);
    }
}

pub type PPResult<T> = Result<T, PPError>;

#[derive(Debug, Error)]
pub enum CError {
    #[error("expected {expected}, got '{got}'")]
    Expected    { span: Span, expected: &'static str, got: String },

    #[error("unknown type '{name}'")]
    UnknownType { span: Span, name: String },

    #[error("unknown field '{name}'")]
    UnknownField { span: Span, name: String },

    #[error("undefined symbol '{name}'")]
    Undefined   { span: Span, name: String },

    #[error("lvalue required")]
    NotLvalue   { span: Span },

    #[error("argument count mismatch for '{name}' (expected {expected})")]
    ArgumentCountMismatch { span: Span, name: String, expected: usize },

    #[error("VLA's can only exist in functions")]
    VlaInGlobalContext { span: Span },

    #[error("variable-length array cannot have an initializer")]
    VlaWithInitializer { span: Span },

    #[error("break outside loop")]
    BreakOutsideLoop { span: Span },

    #[error("continue outside loop")]
    ContinueOutsideLoop { span: Span },

    #[error("register spill - not yet implemented")]
    RegSpill    { span: Span },
}

impl CError {
    #[inline]
    pub fn span(&self) -> Span {
        match self {
            CError::Expected    { span, .. } => *span,
            CError::UnknownType { span, .. } => *span,
            CError::UnknownField { span, .. } => *span,
            CError::Undefined   { span, .. } => *span,
            CError::NotLvalue   { span, .. } => *span,
            CError::ArgumentCountMismatch { span, .. } => *span,
            CError::BreakOutsideLoop { span, .. } => *span,
            CError::ContinueOutsideLoop { span, .. } => *span,
            CError::VlaWithInitializer { span, .. } => *span,
            CError::VlaInGlobalContext { span, .. } => *span,
            CError::RegSpill    { span, .. } => *span,
        }
    }

    #[inline]
    pub fn emit(&self, arena: &SrcArena) {
        emit_diag(&self.to_string(), self.span(), arena);
    }
}

pub type CResult<T> = Result<T, CError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TK {
    Eof, Ident, Number, CharLit, StrLit,
    LSquare, RSquare, LParen, RParen, LCurly, RCurly,
    Comma, Colon, SemiColon, TripleDot,
    Plus, PlusPlus, Minus, MinusMinus, Arrow,
    PlusEq,         MinusEq,
    Less, LessLess, Greater, GreaterGreater,
    LessEq,         GreaterEq,
    Star,   Slash,
    StarEq, SlashEq,
    Eq, EqEq, NotEq,
    Xor, XorEq, Not, BitNot,
    And, Or,
    Dot,
    BinAnd,   BinOr,
    BinAndEq, BinOrEq,

    // PP-internal - never escapes cooked stream

    Hash, Newline, Paste /* ## */, Stringify /* # */,
    // Inside macro bodies only
    Param(u8),
}

impl TK {
    #[inline]
    pub const fn compound_to_binop(self) -> TK {
        match self {
            TK::PlusEq   => TK::Plus,
            TK::MinusEq  => TK::Minus,
            TK::StarEq   => TK::Star,
            TK::SlashEq  => TK::Slash,
            TK::BinAndEq => TK::BinAnd,
            TK::XorEq    => TK::Xor,
            TK::BinOrEq  => TK::BinOr,
            _ => unreachable!(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Token {
    pub kind: TK,
    pub span: Span,
    /// Pre-computed fnv1a hash of the identifier text, set once in cook().
    /// Zero for all non-Ident tokens.
    pub hash: u64,
}

impl Token {
    pub const EOF: Token = Token { kind: TK::Eof, span: Span::POISONED, hash: 0 };

    #[inline]
    pub fn s<'a>(&self, arena: &'a SrcArena) -> &'a str {
        self.span.s(arena)
    }
}

// Pre-computed fnv1a hashes of every keyword.  Used in place of string
// comparisons throughout the compiler hot path - integer equality only.
const HASH_RETURN:   u64 = hash_ident("return");
const HASH_INT:      u64 = hash_ident("int");
const HASH_LONG:     u64 = hash_ident("long");
const HASH_CHAR:     u64 = hash_ident("char");
const HASH_VOID:     u64 = hash_ident("void");
const HASH_FLOAT:    u64 = hash_ident("float");
const HASH_SIGNED:   u64 = hash_ident("signed");
const HASH_UNSIGNED: u64 = hash_ident("unsigned");
const HASH_STATIC:   u64 = hash_ident("static");
const HASH_RESTRICT: u64 = hash_ident("restrict");
const HASH_VOLATILE: u64 = hash_ident("volatile");
const HASH_REGISTER: u64 = hash_ident("register");
const HASH_AUTO:     u64 = hash_ident("auto");
const HASH_INLINE:   u64 = hash_ident("inline");
const HASH_CONST:    u64 = hash_ident("const");
const HASH_DEFINED:  u64 = hash_ident("defined");
const HASH_DOUBLE:   u64 = hash_ident("double");
const HASH_SHORT:    u64 = hash_ident("short");
const HASH_ONCE:     u64 = hash_ident("once");
const HASH_EXTERN:   u64 = hash_ident("extern");
const HASH_DEFINE:   u64 = hash_ident("define");
const HASH_TYPEDEF:  u64 = hash_ident("typedef");
const HASH_STRUCT:   u64 = hash_ident("struct");
const HASH_UNION:    u64 = hash_ident("union");
const HASH_ENUM:     u64 = hash_ident("enum");
const HASH_INCLUDE:  u64 = hash_ident("include");
const HASH_PRAGMA:   u64 = hash_ident("pragma");
const HASH_UNDEF:    u64 = hash_ident("undef");
const HASH_IFNDEF:   u64 = hash_ident("ifndef");
const HASH_IFDEF:    u64 = hash_ident("ifdef");
const HASH_ERROR:    u64 = hash_ident("error");
const HASH_WARNING:  u64 = hash_ident("warning");
const HASH_IF:       u64 = hash_ident("if");
const HASH_FOR:      u64 = hash_ident("for");
const HASH_WHILE:    u64 = hash_ident("while");
const HASH_SIZEOF:   u64 = hash_ident("sizeof");
const HASH_TYPEOF:   u64 = hash_ident("typeof");
const HASH_ELSE:     u64 = hash_ident("else");
const HASH_ELIF:     u64 = hash_ident("elif");
const HASH_ENDIF:    u64 = hash_ident("endif");
const HASH_BREAK:    u64 = hash_ident("break");
const HASH_BOOL:     u64 = hash_ident("bool");
const HASH__BOOL:    u64 = hash_ident("_Bool");
const HASH_CONTINUE: u64 = hash_ident("continue");

const HASH___VA_ARGS__: u64 = hash_ident("__VA_ARGS__");

const HASH_BUILTIN_BSWAP16:     u64 = hash_ident("__builtin_bswap16");
const HASH_BUILTIN_BSWAP32:     u64 = hash_ident("__builtin_bswap32");
const HASH_BUILTIN_BSWAP64:     u64 = hash_ident("__builtin_bswap64");
const HASH_BUILTIN_CLZ:         u64 = hash_ident("__builtin_clz");
const HASH_BUILTIN_CLZL:        u64 = hash_ident("__builtin_clzl");
const HASH_BUILTIN_CLZLL:       u64 = hash_ident("__builtin_clzll");
const HASH_BUILTIN_CTZ:         u64 = hash_ident("__builtin_ctz");
const HASH_BUILTIN_CTZL:        u64 = hash_ident("__builtin_ctzl");
const HASH_BUILTIN_CTZLL:       u64 = hash_ident("__builtin_ctzll");
const HASH_BUILTIN_POPCOUNT:    u64 = hash_ident("__builtin_popcount");
const HASH_BUILTIN_POPCOUNTL:   u64 = hash_ident("__builtin_popcountl");
const HASH_BUILTIN_POPCOUNTLL:  u64 = hash_ident("__builtin_popcountll");
const HASH_BUILTIN_EXPECT:      u64 = hash_ident("__builtin_expect");
const HASH_BUILTIN_UNREACHABLE: u64 = hash_ident("__builtin_unreachable");
const HASH_BUILTIN_TRAP:        u64 = hash_ident("__builtin_trap");

const HASH_HIDDEN_LOCAL: u64 = 0;

const HASHES_THAT_START_TYPES: &[u64] = &[
    HASH_INT, HASH_BOOL, HASH__BOOL, HASH_LONG, HASH_CHAR, HASH_VOID,
    HASH_FLOAT, HASH_DOUBLE, HASH_SHORT,

    HASH_ENUM, HASH_UNION, HASH_STRUCT,
    HASH_TYPEOF, HASH_TYPEDEF,

    // Qualifiers can start a type too...... Sigh.............
    HASH_UNSIGNED, HASH_SIGNED,
    HASH_CONST, HASH_STATIC, HASH_RESTRICT, HASH_VOLATILE, HASH_AUTO, HASH_REGISTER
];

fn lex(src: &[u8], pos: &mut usize, fid: FileRef) -> Token {
    // Skip spaces, tabs, carriage returns.  Written as an iterator so LLVM
    // can auto-vectorise the scan rather than emitting a byte-at-a-time loop.
    let skip = src[*pos..].iter().position(|&b| !matches!(b, b' '|b'\t'|b'\r')).unwrap_or(src.len() - *pos);
    *pos += skip;
    if *pos >= src.len() {
        return Token { kind: TK::Eof, span: Span { file: fid, start: *pos as u32, len: 0 }, hash: 0 };
    }
    let start = *pos;
    let ch = src[*pos]; *pos += 1;

    macro_rules! tok  { ($k:expr) => { Token {
        kind: $k, span: Span { file: fid, start: start as u32, len: (*pos-start) as u16 }, hash: 0
    }}}

    macro_rules! tok2 { ($c:expr, $y:expr, $n:expr) => {{
        if *pos < src.len() && src[*pos]==$c { *pos+=1; tok!($y) } else { tok!($n) }
    }}}

    match ch {
        b'\n' => tok!(TK::Newline),
        b'#'  => tok!(TK::Hash),
        b'['  => tok!(TK::LSquare), b']' => tok!(TK::RSquare),
        b'('  => tok!(TK::LParen),  b')' => tok!(TK::RParen),
        b'{'  => tok!(TK::LCurly),  b'}' => tok!(TK::RCurly),
        b','  => tok!(TK::Comma),   b';' => tok!(TK::SemiColon),
        b':'  => tok!(TK::Colon),
        b'~'  => tok!(TK::BitNot),
        b'*'  => tok2!(b'=', TK::StarEq,    TK::Star),
        b'^'  => tok2!(b'=', TK::XorEq,     TK::Xor),
        b'!'  => tok2!(b'=', TK::NotEq,     TK::Not),
        b'='  => tok2!(b'=', TK::EqEq,      TK::Eq),

        b'<' => {
            if *pos < src.len() && src[*pos] == b'<' { *pos += 1; tok!(TK::LessLess) }
            else if *pos < src.len() && src[*pos] == b'=' { *pos += 1; tok!(TK::LessEq) }
            else { tok!(TK::Less) }
        }
        b'>' => {
            if *pos < src.len() && src[*pos] == b'>' { *pos += 1; tok!(TK::GreaterGreater) }
            else if *pos < src.len() && src[*pos] == b'=' { *pos += 1; tok!(TK::GreaterEq) }
            else { tok!(TK::Greater) }
        }

        b'&' => {
            if *pos < src.len() && src[*pos] == b'&' { *pos += 1; tok!(TK::And) }
            else if *pos < src.len() && src[*pos] == b'=' { *pos += 1; tok!(TK::BinAndEq) }
            else { tok!(TK::BinAnd) }
        }
        b'|' => {
            if *pos < src.len() && src[*pos] == b'|' { *pos += 1; tok!(TK::Or) }
            else if *pos < src.len() && src[*pos] == b'=' { *pos += 1; tok!(TK::BinOrEq) }
            else { tok!(TK::BinOr) }
        }

        b'+' => {
            if *pos < src.len() && src[*pos] == b'+' { *pos += 1; tok!(TK::PlusPlus) }
            else if *pos < src.len() && src[*pos] == b'=' { *pos += 1; tok!(TK::PlusEq) }
            else { tok!(TK::Plus) }
        }
        b'-' => {
            if *pos < src.len() && src[*pos] == b'-' { *pos += 1; tok!(TK::MinusMinus) }
            else if *pos < src.len() && src[*pos] == b'>' { *pos += 1; tok!(TK::Arrow) }
            else if *pos < src.len() && src[*pos] == b'=' { *pos += 1; tok!(TK::MinusEq) }
            else { tok!(TK::Minus) }
        }

        b'.'  => {
            if *pos+1 < src.len() && src[*pos]==b'.' && src[*pos+1]==b'.' {
                *pos += 2; tok!(TK::TripleDot)
            }
            else { tok!(TK::Dot) }
        }

        b'/' => {
            if *pos < src.len() && src[*pos] == b'/' {
                // Line comment

                while *pos < src.len() && src[*pos] != b'\n' { *pos += 1; }
                Token {
                    kind: TK::Newline,
                    span: Span { file: fid, start: start as u32, len: (*pos - start) as u16 },
                    hash: 0,
                }
            } else if *pos + 1 <= src.len() && src[*pos] == b'*' {
                // Block comment

                *pos += 1; // *
                let mut depth: u32 = 1;
                while *pos + 1 < src.len() && depth > 0 {
                    if src[*pos] == b'/' && src[*pos + 1] == b'*' {
                        *pos += 2;
                        depth += 1;
                    } else if src[*pos] == b'*' && src[*pos + 1] == b'/' {
                        *pos += 2;
                        depth -= 1;
                    } else {
                        *pos += 1;
                    }
                }

                if depth > 0 {
                    // Unterminated block comment - consume to EOF
                    *pos = src.len();
                }

                // Tail-call back to get the next real token
                lex(src, pos, fid)
            } else if *pos < src.len() && src[*pos] == b'=' {
                *pos += 1; tok!(TK::SlashEq)
            } else {
                tok!(TK::Slash)
            }
        }

        b'\'' => {
            while *pos < src.len() && src[*pos] != b'\'' {
                if src[*pos] == b'\\' { *pos += 1; }
                *pos += 1;
            }
            if *pos < src.len() { *pos += 1; }
            tok!(TK::CharLit)
        }

        b'"' => {
            while *pos < src.len() && src[*pos] != b'"' && src[*pos] != b'\n' {
                if src[*pos] == b'\\' { *pos += 1; }
                *pos += 1;
            }
            if *pos < src.len() { *pos += 1; }
            tok!(TK::StrLit)
        }

        b'\\' => {
            // Line continuation
            if *pos < src.len() && src[*pos] == b'\n' {
                *pos += 1;
                return lex(src, pos, fid);
            }

            // Otherwise skip unknown character
            lex(src, pos, fid)
        }

        b'0'..=b'9' => {
            while *pos < src.len() && (src[*pos].is_ascii_alphanumeric() || src[*pos]==b'.') { *pos += 1; }
            tok!(TK::Number)
        }

        b'a'..=b'z'|b'A'..=b'Z'|b'_' => {
            while *pos < src.len() && (src[*pos].is_ascii_alphanumeric() || src[*pos]==b'_') { *pos += 1; }
            tok!(TK::Ident)
        }

        _ => lex(src, pos, fid), // Skip unknown byte
    }
}

#[inline]
fn parse_number_int(s: &str) -> i64 {
    let s = s.trim_end_matches(|c| matches!(c, 'u'|'U'|'l'|'L'));  // @Incomplete

    if s.starts_with("0x") || s.starts_with("0X") {
        u64::from_str_radix(&s[2..], 16).unwrap_or(0) as i64
    } else {
        s.parse::<u64>().map(|v| v as i64)
            .or_else(|_| s.parse::<i64>())
            .unwrap_or(0)
    }
}

#[inline]
fn parse_number_float(s: &str) -> f64 {
    let s = if s.ends_with('f') { &s[..s.len()-1] } else { s };
    s.parse().unwrap_or(0.0)
}

const MAX_PARAMS: usize = 8;
const MAX_DEPTH:  usize = 32;

#[repr(C)]
#[derive(Clone, Copy)]
struct MacroDef {
    def_span:     Span, // 8

    body_start:   u32,  // 4
    body_len:     u32,  // 4

    param_count:  u8,   // 1
    is_variadic:  bool, // 1
    _pad:         [u8; 2],
}

struct MacroTable {
    index:    IntMap<u64, u32>,

    scratch:  Vec<Token>,  // Reused across calls, never shrinks

    // Expanded argument pool - reused across expand_func_macro calls.
    // arg_pool holds the cooked tokens for all args of the current call;
    // arg_ends[i] is the exclusive end index in arg_pool for argument i.
    arg_pool: Vec<Token>,
    arg_ends: SmallVec<[u32; MAX_PARAMS]>,

    defs:        SmallVec<[MacroDef; 64]>,
    tok_pool:    SmallVec<[Token; 256]>,
}

impl MacroTable {
    #[inline]
    fn new() -> Self {
        Self {
            defs: SmallVec::new(),
            index: IntMap::with_capacity_and_hasher(64, Default::default()),
            tok_pool: SmallVec::new(),
            arg_ends: SmallVec::new(),
            scratch: Vec::with_capacity(256),
            arg_pool: Vec::with_capacity(256),
        }
    }

    #[inline]
    fn find(&self, hash: u64) -> Option<usize> {
        self.index.get(&hash).map(|&i| i as usize)
    }

    #[inline]
    fn body(&self, index: usize) -> &[Token] {
        let d = &self.defs[index];
        &self.tok_pool[
            d.body_start as usize
            ..
            d.body_start as usize + d.body_len as usize
        ]
    }

    #[inline]
    fn define(&mut self, name_hash: u64, def_span: Span, is_variadic: bool, params: &[u64], body: &[Token]) {
        let body_start  = self.tok_pool.len() as u32;

        self.tok_pool.extend_from_slice(body);

        let def = MacroDef {
            def_span,
            body_start,
            is_variadic,
            body_len:    body.len() as u32,
            param_count: params.len() as u8,
            _pad: [0; 2]
        };

        if let Some(&i) = self.index.get(&name_hash) {
            self.defs[i as usize] = def;
            return;
        }

        let i = self.defs.len() as u32;
        self.index.insert(name_hash, i);
        self.defs.push(def);
    }

    #[inline]
    fn undef(&mut self, hash: u64) {
        self.index.remove(&hash);
    }
}

//
// --- PP ----------------------------------------------------------------------
//
// Two-level pull:
//   raw()  - bytes from file stack or expansion frames
//   cook() - skips newlines, handles directives, expands macros
//
// Public window: pp.current_token / pp.next_token - always cooked, always valid.
// pp.next() slides: cur <- peek <- cook(), returns old cur.
//
// -----------------------------------------------------------------------------
//

struct FileFrame {
    fid: FileRef,
    pos: usize
}

struct Expansions {
    pool:   Vec<Token>,            // flat slab; all active frames are slices within it
    frames: Vec<(u32, u32, u32)>,  // (start, end, cursor) per frame - one Vec for cache locality
}

impl Expansions {
    #[inline]
    fn new() -> Self {
        Self {
            pool:   Vec::with_capacity(1024),
            frames: Vec::with_capacity(32),
        }
    }

    #[inline]
    fn push(&mut self, tokens: impl AsRef<[Token]>) {
        let tokens = tokens.as_ref();
        let start = self.pool.len() as u32;
        let end   = start + tokens.len() as u32;
        self.pool.extend_from_slice(tokens);
        self.frames.push((start, end, start));
    }

    #[inline]
    fn pop(&mut self) {
        if let Some((start, _, _)) = self.frames.pop() {
            self.pool.truncate(start as usize);
        }
    }
}

// Push a macro body into an expansion frame without going through &mut self,
// so the caller can hold a separate borrow on MacroTable at the same time.
#[inline]
fn exp_push_body(exp: &mut Expansions, body: &[Token]) {
    let start = exp.pool.len() as u32;
    let end   = start + body.len() as u32;
    exp.pool.extend_from_slice(body);
    exp.frames.push((start, end, start));
}

#[derive(Clone, Copy, Debug)]
enum SkipResult { Elif, Else, Endif }

#[derive(Clone, Copy, Debug)]
enum IfdefState {
    Active,        // Currently emitting tokens
    Inactive,      // Skipping - condition was false
    Done,          // seen a True branch, skip remaining elif/else
}

pub struct PP {
    current_token:     Token,
    next_token:        Token,

    src_arena:         SrcArena,

    ifdef_stack:       Vec<IfdefState>,
    file_stack:        Vec<FileFrame>,

    exp:               Expansions,
    at_bol:            bx,  // At beginning of line - gate for # directives
    stop_at_newline:   bx,  // For # directives as well

    // TODO(#19): Make pragma_once_paths and include_dirs in PP hashsets
    pragma_once_paths: HashSet<Box<Path>, wyhash::WyHasherBuilder>,
    include_dirs:      Vec<Box<Path>>,

    macros:            MacroTable, // Huge struct (~194 cache lines)
}

impl PP {
    fn new(path: &Path, args: &[String]) -> PPResult<Self> {
        let mut arena = SrcArena::new();
        let fid = arena.add_path(path)?;

        let mut pp = Self {
            src_arena: arena,
            ifdef_stack:        Vec::new(),
            file_stack:         vec![FileFrame { fid, pos: 0 }],
            exp:                Expansions::new(),
            macros:             MacroTable::new(),
            include_dirs: {
                let mut dirs = [
                    "/usr/lib/gcc/x86_64-linux-gnu/14/include",
                    "/usr/lib/gcc/x86_64-linux-gnu/13/include",
                    "/usr/lib/gcc/x86_64-linux-gnu/12/include",
                    "/usr/lib/gcc/x86_64-linux-gnu/11/include",
                    "/usr/lib/gcc/x86_64-linux-gnu/10/include",
                    "/usr/lib/gcc/x86_64-pc-linux-gnu/14/include",
                    "/usr/lib/gcc/x86_64-pc-linux-gnu/13/include",
                    "/usr/lib/gcc/x86_64-pc-linux-gnu/12/include",
                    "/usr/local/include",
                    "/usr/include/x86_64-linux-gnu",
                    "/usr/include",
                ].into_iter().map(str_into_boxed_path).collect::<Vec<_>>();

                for arg in args {
                    //
                    // -Iinclude or -I=BAR args
                    //
                    if let Some(include) = arg.strip_prefix("-I") {
                        let path = if let Some(eq) = include.find('=') {
                            &include[eq+1..]
                        } else {
                            &include
                        };
                        dirs.push(str_into_boxed_path(path));
                    }
                }

                dirs
            },
            pragma_once_paths:  Default::default(),
            at_bol:             true,
            stop_at_newline:    false,
            current_token:      Token::EOF,
            next_token:         Token::EOF,
        };
        pp.init_predefined_macros();
        pp.current_token  = pp.cook();
        pp.next_token = pp.cook();
        Ok(pp)
    }

    #[inline]
    pub fn add_include_dir(&mut self, p: impl Into<Box<Path>>) {
        self.include_dirs.push(p.into());
    }

    #[inline]
    fn init_predefined_macros(&mut self) {
        let predefs: &[(&str, &str)] = &[
            // Standard
            ("__STDC__",                    "1"),
            ("__STDC_VERSION__",            "201112L"),  // C11
            ("__STDC_HOSTED__",             "1"),

            // GNU dialect
            ("__GNUC__",                    "12"),
            ("__GNUC_MINOR__",              "0"),
            ("__GNUC_PATCHLEVEL__",         "0"),
            ("__GNUC_STDC_INLINE__",        "1"),
            ("__GNU_LIBRARY__",             "6"),

            // Architecture
            ("__x86_64__",                  "1"),
            ("__x86_64",                    "1"),
            ("__amd64__",                   "1"),
            ("__amd64",                     "1"),
            ("__k8__",                      "1"),
            ("__k8",                        "1"),
            ("__code_model_small__",        "1"),

            // OS
            ("__linux__",                   "1"),
            ("__linux",                     "1"),
            ("linux",                       "1"),
            ("__unix__",                    "1"),
            ("__unix",                      "1"),
            ("unix",                        "1"),
            ("__ELF__",                     "1"),
            ("__gnu_linux__",               "1"),

            // ABI / sizes
            ("__LP64__",                    "1"),
            ("_LP64",                       "1"),
            ("__SIZEOF_INT__",              "4"),
            ("__SIZEOF_LONG__",             "8"),
            ("__SIZEOF_LONG_LONG__",        "8"),
            ("__SIZEOF_SHORT__",            "2"),
            ("__SIZEOF_POINTER__",          "8"),
            ("__SIZEOF_PTRDIFF_T__",        "8"),
            ("__SIZEOF_SIZE_T__",           "8"),
            ("__SIZEOF_WCHAR_T__",          "4"),
            ("__SIZEOF_WINT_T__",           "4"),
            ("__SIZEOF_FLOAT__",            "4"),
            ("__SIZEOF_DOUBLE__",           "8"),
            ("__SIZEOF_LONG_DOUBLE__",      "16"),

            // Types
            ("__SIZE_TYPE__",               "long unsigned int"),
            ("__PTRDIFF_TYPE__",            "long int"),
            ("__WCHAR_TYPE__",              "int"),
            ("__WINT_TYPE__",               "unsigned int"),
            ("__INTMAX_TYPE__",             "long int"),
            ("__UINTMAX_TYPE__",            "long unsigned int"),
            ("__SIG_ATOMIC_TYPE__",         "int"),
            ("__INT8_TYPE__",               "signed char"),
            ("__INT16_TYPE__",              "short int"),
            ("__INT32_TYPE__",              "int"),
            ("__INT64_TYPE__",              "long int"),
            ("__UINT8_TYPE__",              "unsigned char"),
            ("__UINT16_TYPE__",             "short unsigned int"),
            ("__UINT32_TYPE__",             "unsigned int"),
            ("__UINT64_TYPE__",             "long unsigned int"),
            ("__INTPTR_TYPE__",             "long int"),
            ("__UINTPTR_TYPE__",            "long unsigned int"),

            // Limits
            ("__CHAR_BIT__",                "8"),
            ("__INT_MAX__",                 "2147483647"),
            ("__LONG_MAX__",                "9223372036854775807L"),
            ("__LONG_LONG_MAX__",           "9223372036854775807LL"),
            ("__SHRT_MAX__",                "32767"),
            ("__SCHAR_MAX__",               "127"),
            ("__UCHAR_MAX__",               "255"),
            ("__USHRT_MAX__",               "65535"),
            ("__UINT_MAX__",                "4294967295U"),
            ("__ULONG_MAX__",               "18446744073709551615UL"),
            ("__SIZE_MAX__",                "18446744073709551615UL"),
            ("__PTRDIFF_MAX__",             "9223372036854775807L"),

            // Byte order
            ("__BYTE_ORDER__",              "1234"),
            ("__ORDER_LITTLE_ENDIAN__",     "1234"),
            ("__ORDER_BIG_ENDIAN__",        "4321"),
            ("__ORDER_PDP_ENDIAN__",        "3412"),
            ("__FLOAT_WORD_ORDER__",        "1234"),

            // Misc GCC
            ("__FINITE_MATH_ONLY__",        "0"),
            ("__NO_INLINE__",               "1"),   // we don't inline
            ("__GCC_HAVE_SYNC_COMPARE_AND_SWAP_1", "1"),
            ("__GCC_HAVE_SYNC_COMPARE_AND_SWAP_2", "1"),
            ("__GCC_HAVE_SYNC_COMPARE_AND_SWAP_4", "1"),
            ("__GCC_HAVE_SYNC_COMPARE_AND_SWAP_8", "1"),
            ("__ATOMIC_RELAXED",            "0"),
            ("__ATOMIC_SEQ_CST",            "5"),
            ("__ATOMIC_ACQUIRE",            "2"),
            ("__ATOMIC_RELEASE",            "3"),
            ("__ATOMIC_CONSUME",            "1"),
            ("__ATOMIC_ACQ_REL",            "4"),

            // glibc feature test helpers
            ("__USE_ISOC11",                "1"),
            ("__USE_ISOC99",                "1"),
            ("__USE_ISOC95",                "1"),
            ("__USE_POSIX_IMPLICITLY",      "1"),
            ("__USE_POSIX",                 "1"),
            ("__USE_POSIX2",                "1"),
            ("__USE_POSIX199309",           "1"),
            ("__USE_POSIX199506",           "1"),
            ("__USE_XOPEN2K",               "1"),
            ("__USE_XOPEN2K8",              "1"),
            ("__USE_MISC",                  "1"),
            ("__USE_ATFILE",                "1"),
            ("__USE_FORTIFY_LEVEL",         "0"),
            ("_DEFAULT_SOURCE",             "1"),
            ("_POSIX_C_SOURCE",             "200809L"),
            ("_XOPEN_SOURCE",               "700"),
            ("__GLIBC_USE_ISOC2X",          "0"),
        ];

        for &(name, val) in predefs {
            self.define_simple(name, val);
        }

        // These are used in glibc headers as no-ops or type hints
        // Define them as empty or simple expansions
        let builtins = &[
            ("__attribute__",           ""),  // ignore attributes
            ("__attribute",             ""),
            ("__extension__",           ""),  // ignore gcc extensions
            ("__inline__",              ""),
            ("__inline",                ""),
            ("__restrict",              ""),
            ("__restrict__",            ""),
            ("__volatile__",            "volatile"),
            ("__signed__",              "signed"),
            ("__const",                 "const"),
            ("__const__",               "const"),
        ];
        for &(name, val) in builtins {
            self.define_simple(name, val);
        }

        // @Incomplete
        // @Incomplete
        // @Incomplete

        self.define_noop_func_macro("__attribute__", &["x"]);
        self.define_noop_func_macro("__attribute",   &["x"]);
        self.define_noop_func_macro("__asm__",       &["x"]);
        self.define_noop_func_macro("__asm",         &["x"]);

        self.define_func_macro("__has_feature", &["x"], *b"0");
        self.define_func_macro("__has_extension", &["x"], *b"0");
        self.define_func_macro("__has_attribute", &["x"], *b"0");
        self.define_func_macro("__has_builtin", &["x"], *b"0");
        self.define_func_macro("__has_include", &["x"], *b"0");

        self.define_func_macro("__glibc_clang_prereq",        &["maj", "min"], *b"0");
        self.define_func_macro("__glibc_has_attribute",       &["attr"],       *b"0");
        self.define_func_macro("__glibc_has_builtin",         &["b"],          *b"0");
        self.define_func_macro("__GLIBC_USE",                 &["f"],          *b"0");
        self.define_func_macro("__glibc_clang_has_extension", &["ext"],        *b"0");

        self.define_func_macro("__CONCAT", &["x", "y"],       *b"x ## y");
        self.define_func_macro("__STRING", &["x"],            *b"#x");
    }

    #[inline]
    fn lex_str(&mut self, s: impl AsRef<[u8]>) -> (FileRef, Vec<Token>) {
        let fid  = self.src_arena.add_bytes(&PathBuf::from("<builtin>"), s.as_ref());
        let data = self.src_arena.slice(fid);
        let mut pos  = 0usize;
        let mut toks = Vec::new();
        loop {
            let t = lex(data, &mut pos, fid);
            if t.kind == TK::Eof { break; }
            toks.push(t);
        }
        (fid, toks)
    }

    #[inline]
    pub fn define_simple(&mut self, name: &str, val: &str) {
        let (_, body) = self.lex_str(val.as_bytes());
        self.macros.define(hash_ident(name), Span::POISONED, false, &[], &body);
    }

    #[inline]
    fn define_noop_func_macro(&mut self, name: &str, params: &[&str]) {
        self.define_func_macro(name, params, &[]);
    }

    #[inline]
    fn define_func_macro(&mut self, name: &str, params: &[&str], body: impl AsRef<[u8]>) {
        let (_, body_toks)   = self.lex_str(body.as_ref());
        let param_hashes: SmallVec<[u64; MAX_PARAMS]> = params.iter()
            .map(|&p| hash_ident(p))
            .collect();

        self.macros.define(hash_ident(name), Span::POISONED, false, &param_hashes, &body_toks);
    }

    #[inline]
    fn raw(&mut self) -> Token {
        // Fast path: expansion frames active
        while let Some(frame) = self.exp.frames.last_mut() {
            let (_, end, cursor) = frame;
            if *cursor < *end {
                let t = self.exp.pool[*cursor as usize];
                *cursor += 1;
                return t;
            }
            self.exp.pop();
        }

        // Slow path: read from file stack
        // Most tokens come from here - inline the common case
        if let Some(ff) = self.file_stack.last_mut() {
            let data = self.src_arena.files[ff.fid].data.slice();
            if ff.pos < data.len() {
                return lex(data, &mut ff.pos, ff.fid);
            }
        }

        self.raw_slow()
    }

    #[inline(never)]
    fn raw_slow(&mut self) -> Token {
        loop {
            let Some(ff) = self.file_stack.last_mut() else {
                return Token::EOF;
            };

            let data = self.src_arena.files[ff.fid].data.slice();
            if ff.pos < data.len() {
                return lex(data, &mut ff.pos, ff.fid);
            }

            self.file_stack.pop();
        }
    }

    #[inline]
    fn cook(&mut self) -> Token {
        loop {
            let t = self.raw();
            match t.kind {
                TK::Newline => {
                    self.at_bol = true;
                    if self.stop_at_newline { return t; }
                }

                TK::Hash if self.at_bol => {
                    if let Err(e) = self.directive() {
                        e.emit(&self.src_arena);
                        std::process::exit(1);
                    }
                }

                TK::Hash => {
                    self.at_bol = false;
                    return t;
                }

                TK::Ident => {
                    self.at_bol = false;

                    let hash = if t.hash != 0 {
                        // Already cooked token from exp frame - hash is valid,
                        // and it was already macro-checked when first cooked,
                        // so we can return it directly without another macro lookup
                        return t;
                    } else {
                        hash_ident(t.s(&self.src_arena))
                    };

                    let Some(index) = self.macros.find(hash) else {
                        return Token { kind: t.kind, span: t.span, hash };
                    };

                    let def = self.macros.defs[index];
                    if def.param_count == 0 {
                        let body = self.macros.body(index);
                        exp_push_body(&mut self.exp, body);
                    } else {
                        match self.expand_func_macro(&def, index) {
                            Ok(())  => {}
                            Err(e) => { e.emit(&self.src_arena); std::process::exit(1); }
                        }
                    }
                }

                TK::Eof => return t,

                _ => {
                    self.at_bol = false;
                    return t;
                }
            }
        }
    }

    // Slide window, return consumed token.
    #[inline]
    pub fn next(&mut self) -> Token {
        let t = self.current_token;
        self.current_token  = self.next_token;
        self.next_token = self.cook();
        t
    }

    #[inline]
    pub fn s(&self, t: Token) -> &str { t.s(&self.src_arena) }

    #[inline]
    fn directive(&mut self) -> PPResult<()> {
        let mut name = self.raw();
        if name.kind != TK::Ident {
            self.skip_line();
            return Ok(());
        }

        name.hash = hash_ident(name.s(&self.src_arena));

        match name.hash {
            HASH_DEFINE  => self.pp_define(),
            HASH_INCLUDE => self.pp_include(name.span),
            HASH_PRAGMA  => { self.pp_pragma(); Ok(()) }
            HASH_UNDEF   => { self.pp_undef();  Ok(()) }
            HASH_IFDEF   => self.pp_ifdef(false),
            HASH_IFNDEF  => self.pp_ifdef(true),
            HASH_ELIF    => self.pp_elif(),
            HASH_ELSE    => { self.pp_else(); Ok(()) }
            HASH_ENDIF   => { self.pp_endif(); Ok(()) }
            HASH_ERROR   => self.pp_error(name),
            HASH_WARNING => self.pp_warning(name),
            HASH_IF      => {
                self.ifdef_stack.push(IfdefState::Inactive);

                let val = self.pp_eval_expr()?;
                if val != 0 {
                    *self.ifdef_stack.last_mut().unwrap() = IfdefState::Active;
                } else {
                    match self.skip_branch() {
                        SkipResult::Endif => { self.ifdef_stack.pop(); }
                        SkipResult::Else  => {
                            *self.ifdef_stack.last_mut().unwrap() = IfdefState::Active;
                        }
                        SkipResult::Elif  => { self.pp_elif()?; }
                    }
                }

                Ok(())
            }

            _ => {
                let e = PPError::BadDirective {
                    span: name.span,
                    name: name.s(&self.src_arena).to_owned()
                };
                self.skip_line();
                Err(e)
            }
        }
    }

    fn pp_define(&mut self) -> PPResult<()> {
        let name_tok = self.raw();
        if name_tok.kind != TK::Ident { self.skip_line(); return Ok(()); }

        let name_hash = hash_ident(name_tok.s(&self.src_arena));
        let def_span  = name_tok.span;
        let next      = self.raw();

        let is_func = next.kind == TK::LParen
            && next.span.file  == name_tok.span.file
            && next.span.start == name_tok.span.start + name_tok.span.len as u32;

        let mut param_hashes = SmallVec::<[u64; MAX_PARAMS]>::new();
        let mut is_variadic = false;
        let mut body = Vec::new();

        // Returns TK::Param(i) if t matches a param name, else t unchanged.
        // For __VA_ARGS__, param index = param_count (last slot).
        #[inline]
        fn try_param_subst(t: Token, params: &[u64], is_variadic: bool, arena: &SrcArena) -> Token {
            if t.kind != TK::Ident { return t; }

            let h = hash_ident(t.s(arena));
            for (i, &ph) in params.iter().enumerate() {
                if ph == h {
                    return Token { kind: TK::Param(i as u8), span: t.span, hash: 0 };
                }
            }

            if is_variadic && h == HASH___VA_ARGS__ {
                return Token { kind: TK::Param(params.len() as u8), span: t.span, hash: 0 };
            }

            t
        }

        if is_func {
            loop {
                let t = self.raw();
                match t.kind {
                    TK::RParen            => break,
                    TK::Comma             => {}
                    TK::Ident             => { param_hashes.push(hash_ident(t.s(&self.src_arena))); }
                    TK::TripleDot         => { is_variadic = true; }
                    TK::Newline | TK::Eof => { self.skip_line(); return Ok(()); }
                    _ => {}
                }
            }
        }

        // For object macros `next` is the first body token (or newline/eof).
        // For func macros we start reading fresh.
        let mut pending = if !is_func { Some(next) } else { None };

        loop {
            let t = match pending.take() {
                Some(t) => t,
                None    => self.raw(),
            };

            if matches!(t.kind, TK::Newline | TK::Eof) { self.at_bol = true; break; }

            match t.kind {
                TK::Hash => {
                    let next2 = self.raw();
                    if next2.kind == TK::Hash {
                        // ## paste operator
                        body.push(Token { kind: TK::Paste, span: t.span, hash: 0 });
                    } else if matches!(next2.kind, TK::Newline | TK::Eof) {
                        // # at end of line - ignore
                        self.at_bol = true;
                        break;
                    } else {
                        // # param - stringification
                        let subst = try_param_subst(next2, &param_hashes, is_variadic, &self.src_arena);
                        match subst.kind {
                            TK::Param(pi) => body.push(Token {
                                kind: TK::Stringify,
                                span: t.span,
                                hash: pi as u64,
                            }),
                            _ => {
                                // # non-param: not standard but be permissive
                                body.push(t);
                                body.push(subst);
                            }
                        }
                    }
                }

                _ => body.push(try_param_subst(t, &param_hashes, is_variadic, &self.src_arena)),
            }
        }

        self.macros.define(name_hash, def_span, is_variadic, &param_hashes, &body);

        Ok(())
    }

    #[inline]
    fn pp_undef(&mut self) {
        let t = self.raw();
        if t.kind == TK::Ident {
            self.macros.undef(hash_ident(t.s(&self.src_arena)));
        }

        self.skip_line();
    }

    fn pp_include(&mut self, dir_span: Span) -> PPResult<()> {
        if self.file_stack.len() >= MAX_DEPTH {
            return Err(PPError::IncludeDepth { span: dir_span });
        }

        let first = self.raw();
        let (path_str, is_sys) = match first.kind {
            TK::StrLit => {
                let raw = first.s(&self.src_arena);

                (Cow::Borrowed(&raw[1..raw.len()-1]), false)
            }

            TK::Less => {
                let mut s = String::new();
                loop {
                    let t = self.raw();
                    if matches!(t.kind, TK::Greater | TK::Newline | TK::Eof) { break; }
                    s.push_str(t.s(&self.src_arena));
                }

                (Cow::Owned(s), true)
            }

            _ => { self.skip_line(); return Ok(()); }
        };

        let cur_dir = {
            let fid  = self.file_stack.last().map(|f| f.fid).unwrap_or(FileRef(0));
            let parent_dir_opt = Path::new(self.src_arena.files[fid].path.as_ref()).parent();
            parent_dir_opt.unwrap_or(Path::new(".")).to_owned()
        };

        let resolved = if is_sys {
            self.find_sys(&path_str)
        } else {
            let l = cur_dir.join(path_str.as_ref());
            if l.exists() { Some(l) } else { self.find_sys(&path_str) }
        };

        let Some(resolved) = resolved else {
            let path = path_str.into_owned(); // @Borrow
            self.skip_line();
            return Err(PPError::NotFound { span: first.span, path });
        };

        self.skip_line();

        let resolved = resolved.into_boxed_path();
        if self.pragma_once_paths.contains(&resolved) { return Ok(()); }

        let file = File::open(&resolved)?;
        let mmap = unsafe { MmapOptions::new().map(&file)? };

        let fid = self.src_arena.files.push(FileInfo {
            path: resolved.to_string_lossy().into(),
            data: FileData::Mapped(mmap),
        });
        self.file_stack.push(FileFrame { fid, pos: 0 });

        Ok(())
    }

    #[inline]
    fn find_sys(&self, name: &str) -> Option<PathBuf> {
        self.include_dirs.iter().map(|d| d.join(name)).find(|p| p.exists())
    }

    #[inline]
    fn pp_pragma(&mut self) {
        let t = self.raw();
        if hash_ident(t.s(&self.src_arena)) == HASH_ONCE {
            if let Some(ff) = self.file_stack.last() {
                let path = Path::new(self.src_arena.files[ff.fid].path.as_ref());
                if let Ok(canonical) = path.canonicalize() {
                    self.pragma_once_paths.insert(canonical.into_boxed_path());
                }
            }
        }

        self.skip_line();
    }

    #[inline(never)]
    fn expand_func_macro(&mut self, def: &MacroDef, index: usize) -> PPResult<()> {
        let lp = self.raw();
        if lp.kind != TK::LParen {
            self.exp.push(vec![lp]);
            return Ok(());
        }

        let call_span = lp.span;
        let scratch_base = self.macros.scratch.len() as u32;

        let mut depth     = 0usize;
        let mut arg_index = 0usize;

        // arg_ranges[i] = (start, len) into self.macros.scratch
        let mut arg_ranges = SmallVec::<[_; MAX_PARAMS]>::new();
        let mut arg_start = scratch_base;

        loop {
            let t = self.raw();
            match t.kind {
                TK::Eof => {
                    self.macros.scratch.truncate(scratch_base as usize);
                    return Err(PPError::Unterminated { span: call_span });
                }

                TK::LParen => { depth += 1; self.macros.scratch.push(t); }
                TK::RParen if depth > 0 => { depth -= 1; self.macros.scratch.push(t); }
                TK::RParen => {
                    arg_ranges.push((arg_start, self.macros.scratch.len() as u32 - arg_start));
                    break;
                }

                TK::Comma if depth == 0 => {
                    arg_ranges.push((arg_start, self.macros.scratch.len() as u32 - arg_start));
                    arg_index += 1;

                    let named_params = def.param_count as usize - if def.is_variadic { 1 } else { 0 };

                    if arg_index >= named_params && !def.is_variadic {
                        self.macros.scratch.truncate(scratch_base as usize);
                        return Err(PPError::ArgumentCountMismatch {
                            span: t.span,
                            expected: def.param_count as _,
                            name: def.def_span.s(&self.src_arena).to_owned(),
                        });
                    }

                    if arg_index < named_params {
                        // Normal named param separator - start new range
                        arg_start = self.macros.scratch.len() as u32;
                    } else {
                        // Inside __VA_ARGS__ - keep comma as a token
                        self.macros.scratch.push(t);
                        // Don't update arg_start, all remaining tokens go into the VA range
                    }
                }

                _ => self.macros.scratch.push(t),
            }
        }

        let named_params = def.param_count as usize - if def.is_variadic { 1 } else { 0 };

        if arg_ranges.len() < named_params {
            self.macros.scratch.truncate(scratch_base as usize);
            return Err(PPError::ArgumentCountMismatch {
                span: call_span,
                expected: def.param_count as _,
                name: def.def_span.s(&self.src_arena).to_owned(),
            });
        }

        //
        // If variadic and no extra args: push an empty range for __VA_ARGS__
        //
        if def.is_variadic && arg_ranges.len() == named_params {
            arg_ranges.push((self.macros.scratch.len() as u32, 0));
        }

        let d = &self.macros.defs[index];
        let body_start = d.body_start as usize;
        let body_len   = d.body_len   as usize;

        let all_single_and_not_macros = !def.is_variadic  // Fast path doesn't handle VA
            && arg_ranges.iter().all(|&(start, len)| {
                if len != 1 { return false; }

                let t = self.macros.scratch[start as usize];
                t.kind != TK::Ident || self.macros.find(hash_ident(t.s(&self.src_arena))).is_none()
            })
            && !self.macros.tok_pool[body_start..body_start+body_len].iter()
            .any(|t| matches!(t.kind, TK::Paste | TK::Stringify));

        if all_single_and_not_macros {
            //
            // Fast path: args are single tokens, no need to cook them
            // Just read directly from scratch and substitute into body
            //

            let frame_start = self.exp.pool.len() as u32;
            let mut frame_len = 0u32;

            // Each body token is either a literal token (1) or a Param substituted by 1 token (1)
            // So total pushed = body_len exactly
            self.exp.pool.reserve(body_len);

            for i in body_start..body_start + body_len {
                let t = self.macros.tok_pool[i];
                match t.kind {
                    TK::Param(pi) => {
                        let (arg_s, _) = arg_ranges[pi as usize];
                        self.exp.pool.push(self.macros.scratch[arg_s as usize]);
                    }
                    _ => { self.exp.pool.push(t); }
                }

                frame_len += 1;
            }

            self.macros.scratch.truncate(scratch_base as usize);
            self.exp.frames.push((frame_start, frame_start + frame_len, frame_start));
            return Ok(());
        }

        //
        // Expand each arg from scratch into macros.arg_pool.
        // arg_pool and arg_ends are reset here;
        //

        let arg_pool_base = self.macros.arg_pool.len() as u32;
        self.macros.arg_ends.clear();

        for &(start, len) in &arg_ranges {
            let s = start as usize;
            let e = s + len as usize;
            if s == e { continue; }

            //
            // Copy the arg tokens + EOF sentinel into exp.pool as a temporary frame.
            // We use exp.push so the cook() loop drains it normally.
            //

            let tmp_start = self.exp.pool.len() as u32;
            self.exp.pool.extend_from_slice(&self.macros.scratch[s..e]);
            self.exp.pool.push(Token::EOF);
            let tmp_end = self.exp.pool.len() as u32;

            self.exp.frames.push((tmp_start, tmp_end, tmp_start));

            loop {
                let t = self.cook();
                if t.kind == TK::Eof { break; }
                self.macros.arg_pool.push(t);
            }

            self.macros.arg_ends.push(self.macros.arg_pool.len() as u32);
        }
        self.macros.scratch.truncate(scratch_base as usize);

        //
        // Substitute body directly into exp.pool - read body coords first to
        // release the borrow on self.macros before touching self.exp.
        //

        let frame_start = self.exp.pool.len() as u32;
        let mut frame_len = 0u32;

        let mut i = body_start;
        while i < body_start + body_len {
            let t = self.macros.tok_pool[i];
            i += 1;

            match t.kind {
                TK::Param(pi) => {
                    let pi = pi as usize;

                    let arg_start = if pi == 0 { arg_pool_base as usize }
                                    else       { self.macros.arg_ends[pi - 1] as usize };

                    let arg_end   = self.macros.arg_ends[pi] as usize;

                    self.exp.pool.extend_from_slice(&self.macros.arg_pool[arg_start..arg_end]);
                    frame_len += (arg_end - arg_start) as u32;
                }

                TK::Stringify => {   // @Speed @Memory
                    let pi = t.hash as usize;

                    let arg_start = if pi == 0 { arg_pool_base as usize }
                    else        { self.macros.arg_ends[pi - 1] as usize };

                    let arg_end = self.macros.arg_ends[pi] as usize;

                    let mut s = "\"".to_owned();
                    for tok in &self.macros.arg_pool[arg_start..arg_end] {
                        for ch in tok.s(&self.src_arena).chars() {
                            match ch {
                                '"'  => s.push_str("\\\""),
                                '\\' => s.push_str("\\\\"),
                                _    => s.push(ch),
                            }
                        }
                    }
                    s.push('"');

                    let fid = self.src_arena.add_bytes("<stringify>".as_ref(), s.as_bytes());
                    let data = self.src_arena.slice(fid);

                    let tok = lex(data, &mut 0, fid);
                    self.exp.pool.push(tok);
                    frame_len += 1;
                }

                TK::Paste => {
                    //
                    // Lhs is already pushed, pop it
                    //
                    let lhs_tok = self.exp.pool.pop().unwrap();
                    frame_len -= 1;

                    //
                    // Rhs is the next body token (might itself be a Param)
                    //
                    if i < body_start + body_len {
                        let rhs_tok = self.macros.tok_pool[i];
                        i += 1;

                        let rhs_tok = match rhs_tok.kind {
                            TK::Param(pi) => {
                                // Rhs is a param - get its first token from arg_pool
                                let pi = pi as usize;

                                let arg_start = if pi == 0 { arg_pool_base as usize }
                                else { self.macros.arg_ends[pi-1] as usize };

                                let arg_end  = self.macros.arg_ends[pi] as usize;

                                if arg_start < arg_end { self.macros.arg_pool[arg_start] }
                                else { continue } // Empty arg, discard lhs too?..
                            }
                            _ => rhs_tok
                        };

                        let lhs_text = lhs_tok.s(&self.src_arena);
                        let rhs_text = rhs_tok.s(&self.src_arena);
                        let pasted   = format!("{lhs_text}{rhs_text}");

                        let fid  = self.src_arena.add_bytes("<paste>".as_ref(), pasted.as_bytes());
                        let data = self.src_arena.slice(fid);

                        let tok = lex(data, &mut 0, fid);
                        self.exp.pool.push(tok);

                        frame_len += 1;
                    }

                    // else: ## at end of body, keep lhs as-is
                }

                _ => {
                    self.exp.pool.push(t);
                    frame_len += 1;
                }
            }
        }

        // Release arg_pool back to base - reused by the next call
        self.macros.arg_pool.truncate(arg_pool_base as usize);
        self.exp.frames.push((frame_start, frame_start + frame_len, frame_start));

        Ok(())
    }

    #[inline]
    fn skip_line(&mut self) {
        loop {
            let t = self.raw();
            if matches!(t.kind, TK::Newline | TK::Eof) {
                self.at_bol = true;
                break;
            }
        }
    }

    fn skip_branch(&mut self) -> SkipResult {
        let mut depth = 0usize;

        loop {
            let t = self.raw();

            match t.kind {
                TK::Eof => return SkipResult::Endif,
                TK::Newline => { self.at_bol = true; continue; }

                TK::Hash if self.at_bol => {
                    self.at_bol = false;

                    let name = self.raw();
                    if name.kind != TK::Ident { self.skip_line(); continue; }

                    let h = hash_ident(name.s(&self.src_arena));
                    match h {
                        HASH_IF | HASH_IFDEF | HASH_IFNDEF => {
                            depth += 1;
                            self.skip_line();
                        }

                        HASH_ENDIF => {
                            if depth == 0 {
                                self.skip_line();
                                return SkipResult::Endif;
                            }
                            depth -= 1;
                            self.skip_line();
                        }

                        HASH_ELIF if depth == 0 => {
                            return SkipResult::Elif;
                        }

                        HASH_ELSE if depth == 0 => {
                            self.skip_line();
                            return SkipResult::Else;
                        }

                        _ => self.skip_line(),
                    }
                }

                _ => self.at_bol = false,
            }
        }
    }

    #[inline]
    fn pp_ifdef(&mut self, is_ifndef: bool) -> PPResult<()> {
        let name = self.raw();
        if name.kind != TK::Ident { self.skip_line(); return Ok(()); }

        let hash = hash_ident(name.s(&self.src_arena));
        self.skip_line();

        let defined = self.macros.find(hash).is_some();
        let active  = if is_ifndef { !defined } else { defined };

        if active {
            self.ifdef_stack.push(IfdefState::Active);
        } else {
            self.ifdef_stack.push(IfdefState::Done);
            match self.skip_branch() {
                SkipResult::Endif => { self.ifdef_stack.pop(); }
                SkipResult::Else  => {
                    // Else branch is active
                    *self.ifdef_stack.last_mut().unwrap() = IfdefState::Active;
                }
                SkipResult::Elif  => {
                    // Evaluate elif condition
                    self.pp_elif()?;
                }
            }
        }

        Ok(())
    }

    #[inline]
    fn pp_else(&mut self) {
        self.skip_line();
        match self.ifdef_stack.last() {
            Some(IfdefState::Active) => {
                // We were active (#if was true), now skip the else branch
                *self.ifdef_stack.last_mut().unwrap() = IfdefState::Done;
                self.skip_branch();
                self.ifdef_stack.pop();
            }

            Some(IfdefState::Done) | Some(IfdefState::Inactive) => {
                // Else branch is active
                *self.ifdef_stack.last_mut().unwrap() = IfdefState::Active;
            }

            None => {} // #else without #if - ignore
        }
    }

    #[inline]
    fn pp_elif(&mut self) -> PPResult<()> {
        match self.ifdef_stack.last().copied() {
            Some(IfdefState::Active) => {
                *self.ifdef_stack.last_mut().unwrap() = IfdefState::Done;
                match self.skip_branch() {
                    SkipResult::Endif => { self.ifdef_stack.pop(); }
                    SkipResult::Else  => { self.pp_else(); }
                    SkipResult::Elif  => { self.pp_elif()?; }
                }
            }

            Some(IfdefState::Done) => {
                self.skip_line();
                match self.skip_branch() {
                    SkipResult::Endif => { self.ifdef_stack.pop(); }
                    SkipResult::Else  => { self.pp_else(); }
                    SkipResult::Elif  => { self.pp_elif()?; }
                }
            }

            Some(IfdefState::Inactive) => {
                let val = self.pp_eval_expr()?;
                if val != 0 {
                    *self.ifdef_stack.last_mut().unwrap() = IfdefState::Active;
                } else {
                    match self.skip_branch() {
                        SkipResult::Endif => { self.ifdef_stack.pop(); }
                        SkipResult::Else  => { self.pp_else(); }
                        SkipResult::Elif  => { self.pp_elif()?; }
                    }
                }
            }

            None => {
                // Orphaned #elif - skip the condition line and the branch
                self.skip_line();
                match self.skip_branch() {
                    SkipResult::Endif => {}
                    SkipResult::Else  => { self.skip_branch(); }
                    SkipResult::Elif  => { self.pp_elif()?; }
                }
            }
        }

        Ok(())
    }

    #[inline]
    fn pp_endif(&mut self) {
        self.skip_line();
        self.ifdef_stack.pop();
    }

    #[inline]
    fn pp_error(&mut self, error_token: Token) -> PPResult<()> {
        let mut msg = String::new();
        loop {
            let t = self.raw();
            if matches!(t.kind, TK::Newline | TK::Eof) { break; }

            msg.push_str(t.s(&self.src_arena));
            msg.push(' ');
        }

        Err(PPError::Error { span: error_token.span, msg: msg.trim().to_owned() })
    }

    #[inline]
    fn pp_warning(&mut self, warning_token: Token) -> PPResult<()> {
        let mut msg = String::new();
        loop {
            let t = self.raw();
            if matches!(t.kind, TK::Newline | TK::Eof) { break; }

            msg.push_str(t.s(&self.src_arena));
            msg.push(' ');
        }

        emit_diag_warning(&msg, warning_token.span, &self.src_arena);

        Ok(())
    }

    fn pp_eval_expr(&mut self) -> PPResult<i64> {
        //
        // Collect raw line - stops at newline
        //
        let mut raw_line = Vec::new();
        loop {
            let t = self.raw();
            match t.kind {
                TK::Newline | TK::Eof => { self.at_bol = true; break; }
                _ => raw_line.push(t),
            }
        }

        // Replace defined(X) / defined X with synthetic 0/1 tokens
        let mut processed = self.pp_replace_defined(raw_line);
        processed.push(Token::EOF);

        // Expand macros using frame-only reads, collect tokens
        let frame_depth_before = self.exp.frames.len();
        exp_push_body(&mut self.exp, &processed);

        let toks = self.pp_eval_collect(frame_depth_before);
        Ok(self.eval_const_expr(&toks, &mut 0))
    }

    // Uses pp_eval_raw (frame-only) to avoid consuming real source tokens.
    // Regular raw() would fall through to the file stack if frames are exhausted.
    #[inline]
    fn pp_eval_expand_func(&mut self, index: usize, toks: &mut Vec<Token>, min_depth: usize) {
        let def = self.macros.defs[index];

        // Check for '(' in frames
        let next = self.pp_eval_raw(min_depth);
        if next.kind != TK::LParen {
            toks.push(Token { kind: TK::Number, span: Span::POISONED, hash: 0 });
            return;
        }

        //
        // Collect args from frames only
        //
        let mut args = SmallVec::<[_; 8]>::new();
        let mut current = Vec::new();
        let mut depth = 0usize;
        loop {
            let t = self.pp_eval_raw(min_depth);
            match t.kind {
                TK::Eof => break,
                TK::LParen => { depth += 1; current.push(t); }
                TK::RParen if depth > 0 => { depth -= 1; current.push(t); }
                TK::RParen => { args.push(current.clone()); break; }
                TK::Comma if depth == 0 => { args.push(current.clone()); current.clear(); }
                _ => current.push(t),
            }
        }

        //
        // Substitute and push result as new frame
        //

        let body_start = def.body_start as usize;
        let body_len   = def.body_len   as usize;

        let mut expanded = Vec::new();
        for i in body_start..body_start + body_len {
            let bt = self.macros.tok_pool[i];
            match bt.kind {
                TK::Param(pi) => if (pi as usize) < args.len() {
                    expanded.extend_from_slice(&args[pi as usize]);
                }

                _ => expanded.push(bt),
            }
        }

        expanded.push(Token::EOF);
        exp_push_body(&mut self.exp, &expanded);
    }

    #[inline]
    fn pp_replace_defined(&self, raw_line: Vec<Token>) -> Vec<Token> {
        let mut out = Vec::with_capacity(raw_line.len());

        let mut i = 0;
        while i < raw_line.len() {
            let t = raw_line[i];
            i += 1;

            let t = if t.kind == TK::Ident && t.s(&self.src_arena) == "defined" {
                let has_paren = i < raw_line.len() && raw_line[i].kind == TK::LParen;
                if has_paren { i += 1 }

                let name_hash = if i < raw_line.len() {
                    hash_ident(raw_line[i].s(&self.src_arena))
                } else {
                    0
                };

                if i < raw_line.len() { i += 1 }
                if has_paren && i < raw_line.len() && raw_line[i].kind == TK::RParen { i += 1 }

                let val = self.macros.find(name_hash).is_some() as u64;
                Token { kind: TK::Number, span: Span::POISONED, hash: val }
            } else {
                t
            };

            out.push(t);
        }

        out
    }

    #[inline]
    fn pp_eval_collect(&mut self, frame_depth_before: usize) -> Vec<Token> {
        let mut toks = Vec::new();
        loop {
            let Some(frame) = self.exp.frames.last_mut() else { break; };

            let (_, end, cursor) = frame;
            if *cursor >= *end {
                self.exp.pop();
                if self.exp.frames.len() < frame_depth_before { break; }

                continue;
            }

            let t = self.exp.pool[*cursor as usize];
            *cursor += 1;

            match t.kind {
                TK::Eof => {
                    self.exp.pop();

                    if self.exp.frames.len() < frame_depth_before { break; }
                }

                TK::Ident => {
                    let h = hash_ident(t.s(&self.src_arena));
                    if let Some(index) = self.macros.find(h) {
                        let def = self.macros.defs[index];
                        if def.param_count == 0 {
                            exp_push_body(&mut self.exp, self.macros.body(index));
                        } else {
                            self.pp_eval_expand_func(index, &mut toks, frame_depth_before);
                        }

                        continue;
                    }

                    toks.push(Token { kind: TK::Number, span: Span::POISONED, hash: 0 });
                }

                _ => toks.push(t),
            }
        }

        toks
    }

    // Read one token from frames only, never file stack
    #[inline]
    fn pp_eval_raw(&mut self, min_depth: usize) -> Token {
        loop {
            let Some(frame) = self.exp.frames.last_mut() else {
                return Token::EOF;
            };

            let (_, end, cursor) = frame;
            if *cursor < *end {
                let t = self.exp.pool[*cursor as usize];
                *cursor += 1;
                return t;
            }

            self.exp.pop();
            if self.exp.frames.len() < min_depth {
                return Token::EOF;
            }
        }
    }

    #[inline]
    fn eval_const_expr(&self, toks: &[Token], pos: &mut usize) -> i64 {
        self.eval_or(toks, pos)
    }

    #[inline]
    fn eval_or(&self, toks: &[Token], pos: &mut usize) -> i64 {
        let mut v = self.eval_and(toks, pos);
        while *pos < toks.len() && toks[*pos].kind == TK::Or {
            *pos += 1;
            let r = self.eval_and(toks, pos);
            v = if v != 0 || r != 0 { 1 } else { 0 };
        }

        v
    }

    #[inline]
    fn eval_and(&self, toks: &[Token], pos: &mut usize) -> i64 {
        let mut v = self.eval_bitor(toks, pos);
        while *pos < toks.len() && toks[*pos].kind == TK::And {
            *pos += 1;
            let r = self.eval_bitor(toks, pos);
            v = if v != 0 && r != 0 { 1 } else { 0 };
        }

        v
    }

    #[inline]
    fn eval_bitor(&self, toks: &[Token], pos: &mut usize) -> i64 {
        let mut v = self.eval_bitxor(toks, pos);
        while *pos < toks.len() && toks[*pos].kind == TK::BinOr {
            *pos += 1;
            v |= self.eval_bitxor(toks, pos);
        }

        v
    }

    #[inline]
    fn eval_bitxor(&self, toks: &[Token], pos: &mut usize) -> i64 {
        let mut v = self.eval_bitand(toks, pos);
        while *pos < toks.len() && toks[*pos].kind == TK::Xor {
            *pos += 1;
            v ^= self.eval_bitand(toks, pos);
        }

        v
    }

    #[inline]
    fn eval_bitand(&self, toks: &[Token], pos: &mut usize) -> i64 {
        let mut v = self.eval_eq(toks, pos);
        while *pos < toks.len() && toks[*pos].kind == TK::BinAnd {
            *pos += 1;
            v &= self.eval_eq(toks, pos);
        }

        v
    }

    #[inline]
    fn eval_eq(&self, toks: &[Token], pos: &mut usize) -> i64 {
        let mut v = self.eval_cmp(toks, pos);
        loop {
            if *pos >= toks.len() { break; }

            match toks[*pos].kind {
                TK::EqEq  => { *pos += 1; v = (v == self.eval_cmp(toks, pos)) as i64; }
                TK::NotEq => { *pos += 1; v = (v != self.eval_cmp(toks, pos)) as i64; }
                _ => break,
            }
        }

        v
    }

    #[inline]
    fn eval_cmp(&self, toks: &[Token], pos: &mut usize) -> i64 {
        let mut v = self.eval_shift(toks, pos);
        loop {
            if *pos >= toks.len() { break; }

            match toks[*pos].kind {
                TK::Less      => { *pos += 1; v = (v <  self.eval_add(toks, pos)) as i64; }
                TK::Greater   => { *pos += 1; v = (v >  self.eval_add(toks, pos)) as i64; }
                TK::LessEq    => { *pos += 1; v = (v <= self.eval_add(toks, pos)) as i64; }
                TK::GreaterEq => { *pos += 1; v = (v >= self.eval_add(toks, pos)) as i64; }
                _ => break,
            }
        }

        v
    }

    #[inline]
    fn eval_shift(&self, toks: &[Token], pos: &mut usize) -> i64 {
        let mut v = self.eval_add(toks, pos);
        loop {
            if *pos >= toks.len() { break; }

            match toks[*pos].kind {
                TK::LessLess       => { *pos += 1; let r = self.eval_add(toks, pos); v = v << r; }
                TK::GreaterGreater => { *pos += 1; let r = self.eval_add(toks, pos); v = v >> r; }
                _ => break,
            }
        }

        v
    }

    #[inline]
    fn eval_add(&self, toks: &[Token], pos: &mut usize) -> i64 {
        let mut v = self.eval_mul(toks, pos);
        loop {
            if *pos >= toks.len() { break; }

            match toks[*pos].kind {
                TK::Plus  => { *pos += 1; v += self.eval_mul(toks, pos); }
                TK::Minus => { *pos += 1; v -= self.eval_mul(toks, pos); }
                _ => break,
            }
        }

        v
    }

    #[inline]
    fn eval_mul(&self, toks: &[Token], pos: &mut usize) -> i64 {
        let mut v = self.eval_unary(toks, pos);
        loop {
            if *pos >= toks.len() { break; }

            match toks[*pos].kind {
                TK::Star  => { *pos += 1; v *= self.eval_unary(toks, pos); }
                TK::Slash => { *pos += 1; let r = self.eval_unary(toks, pos); v = if r != 0 { v / r } else { 0 }; }
                _ => break,
            }
        }

        v
    }

    #[inline]
    fn eval_unary(&self, toks: &[Token], pos: &mut usize) -> i64 {
        if *pos >= toks.len() { return 0; }

        match toks[*pos].kind {
            TK::Minus  => { *pos += 1; -self.eval_unary(toks, pos) }
            TK::Not    => { *pos += 1; (self.eval_unary(toks, pos) == 0) as i64 }
            TK::BitNot => { *pos += 1; !self.eval_unary(toks, pos) }
            _          => self.eval_primary(toks, pos),
        }
    }

    #[inline]
    fn eval_primary(&self, toks: &[Token], pos: &mut usize) -> i64 {
        if *pos >= toks.len() { return 0; }

        match toks[*pos].kind {
            TK::Number => {
                let t = toks[*pos];
                *pos += 1;
                if t.span == Span::POISONED {
                    return t.hash as i64;  // Synthetic defined() result from pp_eval_expr
                }

                let s = t.s(&self.src_arena);
                parse_number_int(s)
            }

            TK::Ident => {
                let hash = toks[*pos].hash;
                *pos += 1;

                if hash != HASH_DEFINED {
                    // Undefined identifier = 0 in #if context
                    return 0;
                }

                //
                // `defined(FOO)` or `defined FOO`
                //

                let name_hash = if *pos < toks.len() && toks[*pos].kind == TK::LParen {
                    *pos += 1; // '('
                    let h = if *pos < toks.len() { toks[*pos].hash } else { 0 };
                    *pos += 1; // name
                    if *pos < toks.len() && toks[*pos].kind == TK::RParen { *pos += 1; }
                    h
                } else {
                    let h = if *pos < toks.len() { toks[*pos].hash } else { 0 };
                    *pos += 1;
                    h
                };

                self.macros.find(name_hash).is_some() as i64
            }

            TK::LParen => {
                *pos += 1;

                let v = self.eval_const_expr(toks, pos);
                if *pos < toks.len() && toks[*pos].kind == TK::RParen { *pos += 1; }

                v
            }

            _ => 0,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TypeRef(u32);
entity_impl!(TypeRef);
impl nohash_hasher::IsEnabled for TypeRef {}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct FieldRef(u32);
entity_impl!(FieldRef);

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct FieldEntry {
    pub ty:         TypeRef, // 4 - Field type
    pub offset:     u32,     // 4 - Byte offset within struct/union

    // @Incomplete
    pub bit_offset: u8,      // 1 - Bit offset within byte for bitfields, 0 otherwise
    pub bit_width:  u8,      // 1 - Bit width for bitfields,              0 otherwise

    pub _pad:       u16      // 2
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
pub enum TypeKind {
    Void,
    Bool,
    Char,
    Int,
    Short,
    Long,
    LLong,
    Float,
    Double,
    LDouble,
    Ptr,      // ref_  = pointee TypeRef
    Array,    // ref_  = element TypeRef,  extra = length (0 = unsized/VLA)
    Struct,   //                           extra = field_pool start,          extra2 = field count
    Union,    //                           extra = field_pool start,          extra2 = field count
    Func,     // ref_  = return TypeRef,   extra = param_pool start,          extra2 = param count
    Enum,     // ref_  = repr TypeRef
}

impl TypeKind {
    #[inline]
    pub fn is_signed_int(self, is_unsigned: bool) -> bool {
        self.is_integer() && !is_unsigned
    }

    #[inline]
    pub fn is_float(self) -> bool {
        matches!(self, TypeKind::Float | TypeKind::Double | TypeKind::LDouble)
    }

    #[inline]
    pub fn is_integer(self) -> bool {
        matches!(
            self,
            TypeKind::Bool | TypeKind::Char | TypeKind::Int | TypeKind::Short |
            TypeKind::Long | TypeKind::LLong | TypeKind::Enum
        )
    }

    #[inline]
    pub fn is_ptr(self) -> bool {
        self == TypeKind::Ptr
    }

    #[inline]
    pub fn is_array(self) -> bool {
        self == TypeKind::Array
    }

    #[inline]
    pub fn is_scalar(self) -> bool {
        self.is_integer() || self.is_float() || self.is_ptr()
    }
}

bitflags::bitflags! {
    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
    pub struct QualFlags: u8 {
        const CONST    = 0x1;
        const VOLATILE = 0x2;
        const RESTRICT = 0x4;
        const UNSIGNED = 0x8;
    }
}

bitflags::bitflags! {
    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
    pub struct TypeFlags: u8 {
        const VARIADIC  = 0x01;
        const NORETURN  = 0x02;
        const INLINE    = 0x04;
        const PACKED    = 0x08;
        const FORWARD_DECLARATION = 0x10;
    }
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TypeEntry {
    pub kind:   TypeKind,    // 1 byte
    pub quals:  QualFlags,   // 1 byte
    pub flags:  TypeFlags,   // 1 byte
    pub _pad:   u8,          // 1 byte
    pub ref_:   TypeRef,     // 4 bytes - pointee     / element     / return type
    pub extra:  u32,         // 4 bytes - field start / param start / array len (0 for VLA)
    pub extra2: u32,         // 4 bytes - field count / param count
}

impl Deref for TypeEntry {
    type Target = TypeKind;
    #[inline]
    fn deref(&self) -> &Self::Target { &self.kind }
}

impl DerefMut for TypeEntry {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target { &mut self.kind }
}

impl TypeEntry {
    // Typed accessors with debug assertions
    #[inline]
    #[track_caller]
    pub fn pointee(&self) -> TypeRef {
        debug_assert!(self.kind == TypeKind::Ptr);
        self.ref_
    }

    #[inline]
    pub fn elem(&self) -> TypeRef {
        debug_assert!(self.kind == TypeKind::Array);
        self.ref_
    }

    #[inline]
    pub fn array_len(&self) -> u32 {
        debug_assert!(self.kind == TypeKind::Array);
        self.extra
    }

    #[inline]
    pub fn ret_ty(&self) -> TypeRef {
        debug_assert!(self.kind == TypeKind::Func);
        self.ref_
    }

    #[inline]
    pub fn param_start(&self) -> u32 {
        debug_assert!(self.kind == TypeKind::Func);
        self.extra
    }

    #[inline]
    pub fn param_count(&self) -> u32 {
        debug_assert!(self.kind == TypeKind::Func);
        self.extra2
    }

    #[inline]
    pub fn field_start(&self) -> u32 {
        debug_assert!(matches!(self.kind, TypeKind::Struct | TypeKind::Union));
        self.extra
    }

    #[inline]
    pub fn field_count(&self) -> u32 {
        debug_assert!(matches!(self.kind, TypeKind::Struct | TypeKind::Union));
        self.extra2
    }

    #[inline]
    pub fn is_signed_int(self) -> bool {
        self.kind.is_signed_int(self.is_unsigned())
    }

    #[inline]
    pub fn is_unsigned(&self) -> bool {
        self.quals.contains(QualFlags::UNSIGNED)
    }

    #[inline]
    pub fn is_signed(&self) -> bool {
        !self.is_unsigned()
    }

    #[inline]
    pub fn is_variadic(&self) -> bool {
        self.flags.contains(TypeFlags::VARIADIC)
    }

    #[inline]
    pub fn is_noreturn(&self) -> bool {
        self.flags.contains(TypeFlags::NORETURN)
    }

    #[inline]
    pub fn is_vla(&self) -> bool {
        self.kind == TypeKind::Array && self.extra == 0
    }

    #[inline]
    pub fn vla_dim_off(&self) -> i32 {
        debug_assert!(self.is_vla());
        self.extra2 as i32
    }
}

pub const TYPE_VOID:    TypeRef = TypeRef(0);
pub const TYPE_BOOL:    TypeRef = TypeRef(1);
pub const TYPE_CHAR:    TypeRef = TypeRef(2);
pub const TYPE_UCHAR:   TypeRef = TypeRef(3);
pub const TYPE_SHORT:   TypeRef = TypeRef(4);
pub const TYPE_USHORT:  TypeRef = TypeRef(5);
pub const TYPE_INT:     TypeRef = TypeRef(6);
pub const TYPE_UINT:    TypeRef = TypeRef(7);
pub const TYPE_LONG:    TypeRef = TypeRef(8);
pub const TYPE_ULONG:   TypeRef = TypeRef(9);
pub const TYPE_LLONG:   TypeRef = TypeRef(10);
pub const TYPE_ULLONG:  TypeRef = TypeRef(11);
pub const TYPE_FLOAT:   TypeRef = TypeRef(12);
pub const TYPE_DOUBLE:  TypeRef = TypeRef(13);
pub const TYPE_LDOUBLE: TypeRef = TypeRef(14);

#[inline(always)]
pub const fn is_type_builtin(ty: TypeRef) -> bool {
    ty.0 <= 14
}

#[inline(always)]
pub const fn unsign_a_builtin_type(ty: TypeRef) -> TypeRef {
    match ty {
        TYPE_VOID    => TYPE_VOID,
        TYPE_BOOL    => TYPE_BOOL,
        TYPE_CHAR    => TYPE_UCHAR,
        TYPE_UCHAR   => TYPE_UCHAR,
        TYPE_SHORT   => TYPE_USHORT,
        TYPE_USHORT  => TYPE_USHORT,
        TYPE_INT     => TYPE_UINT,
        TYPE_UINT    => TYPE_UINT,
        TYPE_LONG    => TYPE_ULONG,
        TYPE_ULONG   => TYPE_ULONG,
        TYPE_LLONG   => TYPE_ULLONG,
        TYPE_ULLONG  => TYPE_ULLONG,
        TYPE_FLOAT   => TYPE_FLOAT,
        TYPE_DOUBLE  => TYPE_DOUBLE,
        TYPE_LDOUBLE => TYPE_DOUBLE,

        _ => unsafe { std::hint::unreachable_unchecked() }
    }
}

pub struct TypeName {
    ty: TypeRef,

    //
    // @Memory
    //
    // I'm not sure if we'll ever gonna need to have
    // builtin types with associated type names,
    // but if we didn't, we could use a Span instead.
    //
    s: SmallString<[u8; 23]>
}

impl SparseMapValue<TypeRef> for TypeName {
    #[inline(always)]
    fn key(&self) -> TypeRef { self.ty }
}

pub struct TypeTable {
    // Flat pool - index is TypeRef
    entries: PrimaryMap<TypeRef, TypeEntry>,

    // Open-addressed dedup map: pack_key -> index into entries
    map_keys: Vec<u64>,
    map_vals: Vec<TypeRef>,
    map_mask: usize,
    map_used: usize,

    //
    // Sub-pools
    //

    pub param_pool:  Vec<TypeRef>,    // Func param types

    pub field_pool:  Vec<FieldEntry>,
    pub field_names: Vec<u64>,
    pub field_spans: Vec<Span>,      // Global field index -> span

    //
    // Typenames @Note:
    //
    // Since all types are deduplicated, and typedefs
    // like: `typedef unsigned u32` do NOT produce a new TypeRef,
    // the only way for type names to be correct (without false associations
    // with scalars for example), they need to be associated ONLY with struct/enum/union types,
    // since those can introduce new names, i.e. `typedef struct { ref: u32; } TypeRef`
    //
    pub type_names: SparseMap<TypeRef, TypeName>,
}

impl TypeTable {
    const EMPTY_KEY: u64 = 0;

    const LOAD_NUM:  usize = 3;
    const LOAD_DEN:  usize = 4; // 75% load factor

    #[inline]
    pub fn new() -> Self {
        let cap = 256usize;
        let mut t = Self {
            entries:    PrimaryMap::with_capacity(cap),
            map_keys:   vec![Self::EMPTY_KEY; cap],
            map_vals:   vec![TypeRef(0); cap],
            map_mask:   cap - 1,
            map_used:   0,
            field_spans: Vec::with_capacity(256),
            field_pool: Vec::with_capacity(256),
            field_names: Vec::with_capacity(256),
            param_pool: Vec::with_capacity(256),
            type_names: SparseMap::new()
        };

        // Pre-intern primitives so their TypeRefs are stable constants
        t.init_primitives();
        t
    }

    #[inline]
    fn init_primitives(&mut self) {
        use TypeKind::*;
        use QualFlags as Q;

        for (kind, quals) in [
            (Void,  Q::empty()),    // 0
            (Bool,  Q::empty()),    // 1
            (Char,  Q::empty()),    // 2
            (Char,  Q::UNSIGNED),   // 3
            (Short, Q::empty()),    // 4
            (Short, Q::UNSIGNED),   // 5
            (Int,   Q::empty()),    // 6
            (Int,   Q::UNSIGNED),   // 7
            (Long,  Q::empty()),    // 8
            (Long,  Q::UNSIGNED),   // 9
            (LLong, Q::empty()),    // 10
            (LLong, Q::UNSIGNED),   // 11
            (Float, Q::empty()),    // 12
            (Double,Q::empty()),    // 13
            (LDouble,Q::empty()),   // 14
        ] {
            self.intern(
                kind,
                quals, TypeFlags::empty(),
                TypeRef(0),
                0, 0
            );
        }
    }

    #[inline(always)]
    fn pack_key(e: &TypeEntry) -> u64 {
        let lo = (e.kind as u64)
            | ((e.quals.bits() as u64) << 8)
            | ((e.flags.bits() as u64) << 16)
            | ((e.ref_.0       as u64) << 24)
            | ((e.extra        as u64) << 32)
            | ((e.extra2       as u64) << 48);

        // Finalizer from wyhash/murmur - better avalanche than xor-shift
        let lo = lo ^ (lo >> 30);
        let lo = lo.wrapping_mul(0xbf58476d1ce4e5b9);
        let lo = lo ^ (lo >> 27);
        let lo = lo.wrapping_mul(0x94d049bb133111eb);
        lo ^ (lo >> 31)
    }

    #[inline]
    fn grow(&mut self) {
        let new_cap = (self.map_keys.len() * 2).max(256);
        let new_mask = new_cap - 1;

        let mut new_keys = vec![Self::EMPTY_KEY; new_cap];
        let mut new_vals = vec![TypeRef(0);      new_cap];

        for i in 0..self.map_keys.len() {
            let k = self.map_keys[i];
            if k == Self::EMPTY_KEY { continue; }

            let mut slot = (k as usize) & new_mask;
            loop {
                if new_keys[slot] == Self::EMPTY_KEY {
                    new_keys[slot] = k;
                    new_vals[slot] = self.map_vals[i];
                    break;
                }

                slot = (slot + 1) & new_mask;
            }
        }

        self.map_keys = new_keys;
        self.map_vals = new_vals;
        self.map_mask = new_mask;
    }

    #[inline(never)]
    fn intern_noinline(&mut self, e: TypeEntry, key: u64, start_slot: usize) -> TypeRef {
        let mut slot = start_slot;
        loop {
            let k = self.map_keys[slot];
            if k == Self::EMPTY_KEY {
                // Not found - insert

                if self.map_used * Self::LOAD_DEN >= self.map_keys.len() * Self::LOAD_NUM {
                    self.grow();
                    slot = (key as usize) & self.map_mask;

                    // Re-find slot after grow
                    loop {
                        if self.map_keys[slot] == Self::EMPTY_KEY { break; }
                        slot = (slot + 1) & self.map_mask;
                    }
                }

                let id = TypeRef(self.entries.len() as u32);
                self.entries.push(e);
                self.map_keys[slot] = key;
                self.map_vals[slot] = id;
                self.map_used += 1;

                return id;
            }

            if k == key {
                // Key match - verify full equality to handle hash collisions

                let existing = &self.entries[self.map_vals[slot]];
                if existing.eq(&e) { return self.map_vals[slot]; }
            }

            slot = (slot + 1) & self.map_mask;
        }
    }

    #[inline(always)]
    fn intern_raw(&mut self, e: TypeEntry) -> TypeRef {
        let key = Self::pack_key(&e);
        let key = if key == 0 { 1 } else { key };
        let slot = (key as usize) & self.map_mask;

        // Fast path: check first slot only, no loop
        let k = self.map_keys[slot];
        if k == key {
            let existing = &self.entries[self.map_vals[slot]];
            if existing.eq(&e) {
                return self.map_vals[slot];
            }
        }

        self.intern_noinline(e, key, slot)
    }

    #[inline(always)]
    pub fn intern(
        &mut self,
        kind: TypeKind,
        quals: QualFlags, flags: TypeFlags,
        ref_: TypeRef,
        extra: u32, extra2: u32
    ) -> TypeRef {
        let e = TypeEntry { kind, quals, flags, ref_, _pad: 0, extra, extra2 };
        self.intern_raw(e)
    }

    #[inline(always)]
    pub fn alloc_fresh(
        &mut self, kind: TypeKind,
        quals: QualFlags, flags: TypeFlags,
        ref_: TypeRef,
        extra: u32, extra2: u32
    ) -> TypeRef {
        let e = TypeEntry { kind, quals, flags, ref_, _pad: 0, extra, extra2 };
        self.entries.push(e)
    }

    #[inline]
    pub fn qualify(&mut self, ty: TypeRef, quals: QualFlags) -> TypeRef {
        if quals.is_empty() { return ty; }

        let e = *self.get(ty);
        self.intern(e.kind, e.quals | quals, e.flags, e.ref_, e.extra, e.extra2)
    }

    #[inline]
    pub fn set_type_name(&mut self, ty: TypeRef, name: impl Into<SmallString<[u8; 23]>>) {
        if is_type_builtin(ty) { return; }   // Just reject it....

        self.type_names.insert(TypeName { ty, s: name.into() });
    }

    #[inline]
    pub fn get_type_name(&self, ty: TypeRef) -> Option<&str> {
        self.type_names.get(ty).map(|e| e.s.as_str())
    }

    #[inline]
    pub fn associate_type_name(&mut self, parent: TypeRef, child: TypeRef) {   // @Memory
        if is_type_builtin(parent) || is_type_builtin(child) { return; }   // Just reject it....

        let Some(s) = self.get_type_name(parent) else { return; };

        self.type_names.insert(TypeName { ty: child, s: s.into() });
    }

    #[inline]
    pub fn get_param(&self, func_ty: TypeRef, param_index: u32) -> TypeRef {
        let e = self.get(func_ty);
        self.param_slice(e.param_start() as _, e.param_count() as _)[param_index as usize]
    }

    #[inline]
    pub fn get_field_from_start(&self, start: u32, field_index: u32) -> &FieldEntry {
        &self.field_pool[start as usize + field_index as usize]
    }

    #[inline]
    pub fn get(&self, id: TypeRef) -> &TypeEntry {
        &self.entries[id]
    }

    #[inline]
    pub fn get_kind(&self, id: TypeRef) -> TypeKind {
        self.entries[id].kind
    }

    /// If `ty` is a function pointer or bare function type, returns the Func TypeRef.
    #[inline]
    fn resolve_func_ptr_type(&self, ty: TypeRef) -> Option<TypeRef> {
        match self.get_kind(ty) {
            TypeKind::Func => Some(ty),
            TypeKind::Ptr  => {
                let inner = self.deref(ty);
                if self.get_kind(inner) == TypeKind::Func { Some(inner) } else { None }
            }
            _ => None,
        }
    }

    #[inline]
    pub fn array_of(&mut self, elem: TypeRef, len: u32) -> TypeRef {
        self.intern(TypeKind::Array, QualFlags::empty(), TypeFlags::empty(), elem, len, 0)
    }

    #[inline]
    pub fn is_integer(&self, id: TypeRef) -> bool {
        matches!(self.get(id).kind, TypeKind::Bool | TypeKind::Int | TypeKind::Long | TypeKind::LLong)
    }

    #[inline]
    pub fn is_float(&self, id: TypeRef) -> bool {
        matches!(self.get(id).kind, TypeKind::Float | TypeKind::Double | TypeKind::LDouble)
    }

    #[inline]
    pub fn is_ptr(&self, id: TypeRef) -> bool {
        self.get(id).kind == TypeKind::Ptr
    }

    #[inline]
    pub fn is_unsigned(&self, id: TypeRef) -> bool {
        self.get(id).is_unsigned()
    }

    #[inline]
    pub fn is_signed(&self, id: TypeRef) -> bool {
        self.get(id).is_signed()
    }

    #[inline]
    pub fn ptr_to(&mut self, pointee: TypeRef) -> TypeRef {
        self.intern(
            TypeKind::Ptr,
            QualFlags::empty(),
            TypeFlags::empty(),
            pointee,
            0,
            0,
        )
    }

    #[inline]
    pub fn deref(&self, id: TypeRef) -> TypeRef {
        let e = self.get(id);
        debug_assert!(e.kind == TypeKind::Ptr);
        e.ref_   // just follow ref_ - no depth arithmetic
    }

    #[inline]
    pub fn size_of(&self, id: TypeRef) -> u32 {
        let e = self.get(id);
        match e.kind {
            TypeKind::Void                                => 0,
            TypeKind::Short                               => 2,
            TypeKind::Bool | TypeKind::Char               => 1,
            TypeKind::Int                                 => 4,
            TypeKind::Long | TypeKind::LLong              => 8,
            TypeKind::Float                               => 4,
            TypeKind::Double                              => 8,
            TypeKind::LDouble                             => 16,
            TypeKind::Ptr                                 => 8,
            TypeKind::Enum                                => 4,
            TypeKind::Array  => self.size_of(e.elem()) * e.array_len(),
            TypeKind::Struct => self.struct_size(e.field_start(), e.field_count()),
            TypeKind::Union  => self.union_size(e.field_start(), e.field_count()),
            TypeKind::Func                                => 0,
        }
    }

    #[inline]
    pub fn align_of(&self, id: TypeRef) -> u32 {
        let e = self.get(id);
        match e.kind {
            TypeKind::Void | TypeKind::Func               => 1,
            TypeKind::Bool | TypeKind::Char               => 1,
            TypeKind::Short                               => 2,
            TypeKind::Int                                 => 4,
            TypeKind::Long | TypeKind::LLong              => 8,
            TypeKind::Float                               => 4,
            TypeKind::Double                              => 8,
            TypeKind::LDouble                             => 16,
            TypeKind::Ptr                                 => 8,
            TypeKind::Enum                                => 4,
            TypeKind::Array  => self.align_of(e.elem()),
            TypeKind::Struct => self.struct_align(e.field_start(), e.field_count()),
            TypeKind::Union  => self.union_align(e.field_start(), e.field_count()),
        }
    }

    #[inline]
    pub fn alloc_fields(&mut self, fields: &[FieldEntry], field_names: &[u64], field_spans: &[Span]) -> (u32, u32) {
        let start = self.field_pool.len() as u32;
        self.field_pool.extend_from_slice(fields);
        self.field_names.extend_from_slice(field_names);
        self.field_spans.extend_from_slice(field_spans);
        (start, fields.len() as u32)
    }

    #[inline]
    pub fn field_slice(&self, start: u32, count: u32) -> &[FieldEntry] {
        let s = start as usize;
        let e = s + count as usize;
        &self.field_pool[s..e]
    }

    #[inline]
    pub fn field_span_slice(&self, start: u32, count: u32) -> &[Span] {
        let s = start as usize;
        let e = s + count as usize;
        &self.field_spans[s..e]
    }

    #[inline]
    pub fn field_name_slice(&self, start: u32, count: u32) -> &[u64] {
        let s = start as usize;
        let e = s + count as usize;
        &self.field_names[s..e]
    }

    #[inline]
    pub fn iter_fields_with_names(&self, start: u32, count: u32) -> impl Iterator<Item = (&FieldEntry, Span)> {
        self.field_slice(start, count).iter().zip(self.field_span_slice(start, count).iter().copied())
    }

    #[inline]
    pub fn find_field(&self, start: u32, count: u32, name: u64) -> Option<&FieldEntry> {
        let relative_index = self.field_name_slice(start, count).iter()
            .position(|_name| *_name == name)?;

        Some(&self.field_slice(start, count)[relative_index])
    }

    #[inline]
    pub fn patch_struct_forward_decl(&mut self, ty: TypeRef, start: u32, count: u32, is_union: bool) {
        let e = &mut self.entries[ty];
        e.kind   = if is_union { TypeKind::Union } else { TypeKind::Struct };
        e.flags  = TypeFlags::empty();
        e.extra  = start;
        e.extra2 = count;
    }

    #[inline]
    pub fn patch_func_forward_decl(&mut self, ty: TypeRef, flags: TypeFlags) {
        let e = &mut self.entries[ty];
        e.flags = flags;
    }

    #[inline]
    pub fn patch_enum_forward_decl(&mut self, ty: TypeRef) {
        let e = &mut self.entries[ty];
        e.flags = TypeFlags::empty();
    }

    // Lay out a struct: compute offsets respecting alignment, return (size, align).
    // Mutates the field_pool entries in-place to fill in offsets.
    #[inline]
    pub fn layout_struct(&mut self, start: u32, count: u32) -> (u32, u32) {
        let mut offset    = 0u32;
        let mut max_align = 1u32;

        for i in 0..count as usize {
            let idx = start as usize + i;
            let ty  = self.field_pool[idx].ty;
            let field_align = self.align_of(ty);
            let field_size  = self.size_of(ty);

            // Pad up to field alignment
            offset = align(offset as _, field_align as _) as _;
            max_align = max_align.max(field_align);

            self.field_pool[idx].offset = offset;
            offset += field_size;
        }

        // Final size padded to struct alignment
        let size = align(offset as _, max_align as _) as _;
        (size, max_align)
    }

    // Lay out a union: all fields start at offset 0, size = largest field.
    // Mutates field_pool in-place.
    #[inline]
    pub fn layout_union(&mut self, start: u32, count: u32) -> (u32, u32) {
        // @Cutnpaste from layout_struct

        let mut max_size:  u32 = 0;
        let mut max_align: u32 = 1;

        for i in 0..count as usize {
            let idx = start as usize + i;
            let ty  = self.field_pool[idx].ty;
            let field_align = self.align_of(ty);
            let field_size  = self.size_of(ty);

            // All fields at offset 0
            self.field_pool[idx].offset = 0;

            max_size  = max_size.max(field_size);
            max_align = max_align.max(field_align);
        }

        // Union size padded to alignment
        let size = align(max_size as _, max_align as _) as _;
        (size, max_align)
    }

    //
    // Size/align via stored layout
    // Called from size_of/align_of after layout is done
    //

    #[inline]
    pub fn struct_size(&self, start: u32, count: u32) -> u32 {
        if count == 0 { return 0; }

        // Size = offset of last field + size of last field, padded to alignment
        let fields = self.field_slice(start, count);

        let last       = &fields[count as usize - 1];
        let last_size  = self.size_of(last.ty);
        let last_end   = last.offset + last_size;
        let max_align  = self.struct_align(start, count);
        align(last_end as _, max_align as _) as _
    }

    #[inline]
    pub fn struct_align(&self, start: u32, count: u32) -> u32 {
        self.field_slice(start, count)
            .iter()
            .map(|f| self.align_of(f.ty))
            .max()
            .unwrap_or(1)
    }

    #[inline]
    pub fn union_size(&self, start: u32, count: u32) -> u32 {
        let max_size  = self.field_slice(start, count)
            .iter()
            .map(|f| self.size_of(f.ty))
            .max()
            .unwrap_or(0);

        let max_align = self.union_align(start, count);
        align(max_size as _, max_align as _) as _
    }

    #[inline]
    pub fn union_align(&self, start: u32, count: u32) -> u32 {
        self.field_slice(start, count)
            .iter()
            .map(|f| self.align_of(f.ty))
            .max()
            .unwrap_or(1)
    }

    // Call after layout_struct to register the type
    #[inline]
    pub fn make_struct_with_flags(&mut self, field_start: u32, field_count: u32, flags: TypeFlags) -> TypeRef {
        self.intern(
            TypeKind::Struct,
            QualFlags::empty(),
            flags,
            TypeRef(0),
            field_start,
            field_count,
        )
    }

    // Call after layout_struct to register the type
    #[inline]
    pub fn make_struct(&mut self, field_start: u32, field_count: u32) -> TypeRef {
        self.intern(
            TypeKind::Struct,
            QualFlags::empty(),
            TypeFlags::empty(),
            TypeRef(0),
            field_start,
            field_count,
        )
    }

    #[inline]
    pub fn make_union(&mut self, field_start: u32, field_count: u32) -> TypeRef {
        self.intern(
            TypeKind::Union,
            QualFlags::empty(),
            TypeFlags::empty(),
            TypeRef(0),
            field_start,
            field_count,
        )
    }

    #[inline]
    pub fn make_func(&mut self, ret: TypeRef, param_start: u32, param_count: u32, variadic: bool) -> TypeRef {
        self.intern(
            TypeKind::Func,
            QualFlags::empty(),
            if variadic { TypeFlags::VARIADIC } else { TypeFlags::empty() },
            ret,
            param_start,
            param_count,
        )
    }

    #[inline]
    pub fn alloc_params(&mut self, params: &[TypeRef]) -> u32 {
        let start = self.param_pool.len() as u32;
        self.param_pool.extend_from_slice(params);
        start
    }

    #[inline]
    pub fn param_slice(&self, start: u32, count: u32) -> &[TypeRef] {
        let s = start as usize;
        &self.param_pool[s..s + count as usize]
    }

    //
    //
    //
    // The biggest sigh in history, because WHO THE FUCK CAME UP WITH THIS FUCKING GRAMMAR???????
    //
    // Sigh......................................................................
    // ..........................................................................
    // ..........................................................................
    // ..........................................................................
    // ..........................................................................
    // ..........................................................................
    // ..........................................................................
    // ..........................................................................
    // ..........................................................................
    // ..........................................................................
    // ..........................................................................
    // ..........................................................................
    // ..........................................................................
    // ..........................................................................
    // ..........................................................................
    // ..........................................................................
    // ..........................................................................
    // ..........................................................................
    // ..........................................................................
    // ..........................................................................
    // ..........................................................................
    // ..........................................................................
    // ..........................................................................
    // ..........................................................................
    // ..........................................................................
    // ..........................................................................
    // ..........................................................................
    // ..........................................................................
    // ..........................................................................
    // ..........................................................................
    //
    //
    //

    fn build_type_parts(&self, id: TypeRef, left: &mut String, right: &mut String) {
        use core::fmt::Write as _;
        use TypeKind::*;

        let e = self.get(id);

        if let Some(type_name) = self.get_type_name(id) {
            // @Cutnpaste from below
            if e.quals.contains(QualFlags::CONST)    { left.push_str("const ");    }
            if e.quals.contains(QualFlags::VOLATILE) { left.push_str("volatile "); }
            if e.quals.contains(QualFlags::UNSIGNED) { left.push_str("unsigned "); }

            left.push_str(type_name);

            return;
        }

        match e.kind {
            Void  | Bool   | Char    | Int    | Short | Long | LLong |
            Float | Double | LDouble | Struct | Union | Enum => {
                if e.quals.contains(QualFlags::CONST)    { left.push_str("const ");    }
                if e.quals.contains(QualFlags::VOLATILE) { left.push_str("volatile "); }
                if e.quals.contains(QualFlags::UNSIGNED) { left.push_str("unsigned "); }

                left.push_str(match e.kind {
                    Void    => "void",
                    Bool    => "bool",
                    Char    => "char",
                    Int     => "int",
                    Short   => "short",
                    Long    => "long",
                    LLong   => "long long",
                    Float   => "float",
                    Double  => "double",
                    LDouble => "long double",
                    Struct  => self.get_type_name(id).unwrap_or("struct <anonymous>"),
                    Union   => self.get_type_name(id).unwrap_or("union <anonymous>"),
                    Enum    => self.get_type_name(id).unwrap_or("enum <anonymous>"),
                    _       => unsafe { std::hint::unreachable_unchecked() }
                });
            }

            Ptr => {
                let mut ptr_tok = "*".to_owned();
                if e.quals.contains(QualFlags::CONST)    { ptr_tok.push_str(" const");    }
                if e.quals.contains(QualFlags::VOLATILE) { ptr_tok.push_str(" volatile"); }
                if e.quals.contains(QualFlags::RESTRICT) { ptr_tok.push_str(" restrict"); }

                let pointee = e.pointee();
                if matches!(self.get(pointee).kind, Array | Func) {
                    //
                    // Pointer to array/func - needs parens: int (*)[10]
                    //

                    let mut inner_left  = String::new();
                    let mut inner_right = String::new();
                    self.build_type_parts(pointee, &mut inner_left, &mut inner_right);

                    _ = write!(left, "{inner_left} ({ptr_tok}");
                    right.insert_str(0, &format!("){inner_right}"));
                } else {
                    self.build_type_parts(pointee, left, right);
                    _ = write!(left, " {ptr_tok}");
                }
            }

            Array => {
                let len = e.array_len();
                let arr_tok = if len > 0 { format!("[{len}]") } else { String::from("[]") };
                right.insert_str(0, &arr_tok);
                self.build_type_parts(e.elem(), left, right);
            }

            Func => {
                //
                // Build param list into right: (int, float, ...)
                //

                let mut params = "(".to_owned();

                let start = e.param_start() as usize;
                let count = e.param_count() as usize;
                for i in 0..count {
                    if i > 0 { params.push_str(", "); }
                    let param_ty = self.param_pool[start + i];
                    let mut pl = String::new();
                    let mut pr = String::new();
                    self.build_type_parts(param_ty, &mut pl, &mut pr);
                    params.push_str(&pl);
                    params.push_str(&pr);
                }

                if e.flags.contains(TypeFlags::VARIADIC) {
                    if count > 0 { params.push_str(", "); }
                    params.push_str("...");
                }

                params.push(')');

                right.insert_str(0, &params);
                self.build_type_parts(e.ret_ty(), left, right);
            }
        }
    }

    #[inline]
    pub fn display_type(&self, id: TypeRef, f: &mut dyn core::fmt::Write) -> core::fmt::Result {
        let mut left  = String::new();
        let mut right = String::new();
        self.build_type_parts(id, &mut left, &mut right);
        write!(f, "{left}{right}")
    }

    #[inline]
    pub fn to_string(&self, id: TypeRef) -> String {
        let mut s = String::new();
        self.display_type(id, &mut s).unwrap();
        s
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XmmReg {
    Xmm0 = 0, Xmm1 = 1, Xmm2 = 2, Xmm3 = 3, Xmm4 = 4, Xmm5 = 5, Xmm6 = 6, Xmm7 = 7
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reg {
    Rax = 0, Rcx = 1, Rdx = 2,  Rbx = 3,  Rsp = 4,  Rbp = 5,  Rsi = 6,  Rdi = 7,
    R8  = 8, R9 = 9,  R10 = 10, R11 = 11, R12 = 12, R13 = 13, R14 = 14, R15 = 15,
}

impl Reg {
    #[inline]
    pub const fn enc(self) -> u8 { self as u8 & 7 }

    #[inline]
    pub const fn ext(self) -> bool { self as u8 >= 8 }
}

const SCRATCH:  &[Reg] = &[Reg::Rax, Reg::Rcx, Reg::Rdx, Reg::R8, Reg::R9, Reg::R10, Reg::R11];
const ARG_REGS: &[Reg] = &[Reg::Rdi, Reg::Rsi, Reg::Rdx, Reg::Rcx, Reg::R8, Reg::R9];
const XMM_ARG_REGS: &[XmmReg] = &[
    XmmReg::Xmm0, XmmReg::Xmm1, XmmReg::Xmm2, XmmReg::Xmm3,
    XmmReg::Xmm4, XmmReg::Xmm5, XmmReg::Xmm6, XmmReg::Xmm7,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValReg { Gp(Reg), Xmm(XmmReg) }

impl ValReg {
    #[inline]
    pub const fn as_gp(self) -> Reg {
        match self {
            Self::Gp(gp) => gp,
            _ => unreachable!()
        }
    }

    #[inline]
    pub const fn as_xmm(self) -> XmmReg {
        match self {
            Self::Xmm(xmm) => xmm,
            _ => unreachable!()
        }
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VK { Imm, Reg, Local, RegInd }

/// Packed byte: bits [6..5] = VK, bit [4] = is_xmm, bits [3..0] = reg index
///
///  7    6  5    4      3  2  1  0
/// [unused][VK: 2][is_xmm][reg: 4]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackedKindReg(u8);

impl PackedKindReg {
    #[inline]
    pub const fn new(kind: VK, reg: ValReg) -> Self {
        let (is_xmm, idx) = match reg {
            ValReg::Gp(r)  => (0u8, r as u8),
            ValReg::Xmm(r) => (1u8, r as u8),
        };
        Self((kind as u8) << 5 | is_xmm << 4 | idx)
    }

    #[inline]
    pub const fn kind(self) -> VK {
        match self.0 >> 5 {
            0 => VK::Imm,
            1 => VK::Reg,
            2 => VK::Local,
            3 => VK::RegInd,
            _ => unsafe { std::hint::unreachable_unchecked() }
        }
    }

    #[inline]
    pub const fn reg(self) -> ValReg {
        let idx = self.0 & 0xF;
        if self.0 & 0x10 != 0 {
            ValReg::Xmm(unsafe { std::mem::transmute::<u8, XmmReg>(idx) })
        } else {
            ValReg::Gp(unsafe { std::mem::transmute::<u8, Reg>(idx) })
        }
    }

    #[inline]
    pub const fn as_gp(self) -> Reg {
        unsafe { std::mem::transmute::<u8, Reg>(self.0 & 0xF) }
    }

    #[inline]
    pub const fn as_xmm(self) -> XmmReg {
        unsafe { std::mem::transmute::<u8, XmmReg>(self.0 & 0xF) }
    }
}

//
// CValue - value stack entry ----------------------------------------------
//
//   Imm    - compile-time constant
//   Reg    - value in register
//   Local  - [rbp + offset]
//   RegInd - [reg + offset] (register indirection: deref, array, struct field)
//

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CValue {
    /// kind==Imm           :  integer immediate (or f64 bits)
    /// kind==Local|RegInd  :  low 32 bits = offset as i32, upper 32 unused
    /// kind==Reg           :  unused
    pub imm:    i64,            // 8

    pub ty:     TypeRef,        // 4
    pub kr:     PackedKindReg,  // 1
}

impl CValue {
    #[inline] pub const fn kind(self)   -> VK     { self.kr.kind() }
    #[inline] pub const fn reg(self)    -> ValReg { self.kr.reg()  }
    #[inline] pub const fn as_gp(self)  -> Reg    { self.kr.as_gp()  }
    #[inline] pub const fn as_xmm(self) -> XmmReg { self.kr.as_xmm() }

    #[inline] pub const fn get_fimm(self)   -> f64 { f64::from_bits(self.imm as u64) }
    #[inline] pub const fn offset(self) -> i32 { self.imm as i32 }

    #[inline]
    pub const fn imm(ty: TypeRef, imm: i64) -> Self {
        Self { imm, ty, kr: PackedKindReg::new(VK::Imm, ValReg::Gp(Reg::Rax)) }
    }

    #[inline]
    pub const fn fimm(ty: TypeRef, v: f64) -> Self {
        Self { imm: v.to_bits() as i64, ty, kr: PackedKindReg::new(VK::Imm, ValReg::Gp(Reg::Rax)) }
    }

    #[inline]
    pub const fn gp(ty: TypeRef, reg: Reg) -> Self {
        Self { imm: 0, ty, kr: PackedKindReg::new(VK::Reg, ValReg::Gp(reg)) }
    }

    #[inline]
    pub const fn xmm(ty: TypeRef, reg: XmmReg) -> Self {
        Self { imm: 0, ty, kr: PackedKindReg::new(VK::Reg, ValReg::Xmm(reg)) }
    }

    #[inline]
    pub const fn local(ty: TypeRef, offset: i32) -> Self {
        Self { imm: offset as i64, ty, kr: PackedKindReg::new(VK::Local, ValReg::Gp(Reg::Rbp)) }
    }

    #[inline]
    pub const fn regind(ty: TypeRef, reg: Reg, offset: i32) -> Self {
        Self { imm: offset as i64, ty, kr: PackedKindReg::new(VK::RegInd, ValReg::Gp(reg)) }
    }

    #[inline]
    pub const fn is_lvalue(self) -> bool {
        matches!(self.kind(), VK::Local | VK::RegInd)
    }
}

pub struct XmmAlloc { used: u8 }

impl XmmAlloc {
    #[inline]
    pub fn new() -> Self { Self { used: 0 } }

    #[inline]
    pub fn alloc(&mut self, span: Span) -> CResult<XmmReg> {
        for i in 0..8u8 {
            if self.used & (1 << i) == 0 {
                self.used |= 1 << i;
                return Ok(unsafe { core::mem::transmute(i) });
            }
        }

        Err(CError::RegSpill { span })
    }

    #[inline]
    pub fn free(&mut self, r: XmmReg) {
        self.used &= !(1 << r as u8);
    }

    #[inline]
    pub fn mark(&mut self, r: XmmReg) {
        self.used |=  1 << r as u8;
    }

    #[inline]
    pub fn clobber_caller_save(&mut self) {
        self.used = 0;
    }
}

pub struct RegAlloc { used: u16 }

impl RegAlloc {
    #[inline]
    pub fn new() -> Self { Self { used: 0 } }

    #[inline]
    pub fn alloc(&mut self, span: Span) -> CResult<Reg> {
        for (i, &r) in SCRATCH.iter().enumerate() {
            if self.used & (1 << i) == 0 { self.used |= 1 << i; return Ok(r); }
        }
        Err(CError::RegSpill { span })
    }

    #[inline]
    pub fn free(&mut self, r: Reg) {
        if let Some(i) = SCRATCH.iter().position(|&x| x == r) { self.used &= !(1 << i); }
    }

    #[inline]
    pub fn mark(&mut self, r: Reg) {
        if let Some(i) = SCRATCH.iter().position(|&x| x == r) { self.used |= 1 << i; }
    }

    #[inline]
    pub fn clobber_caller_save(&mut self) {
        self.used = 0;
    }
}

pub struct CodeBuf {
    pub bytes: Vec<u8>
}

impl CodeBuf {
    #[inline]
    pub fn new() -> Self {
        Self { bytes: Vec::with_capacity(4 * 1024 * 1024) }
    }

    #[inline]
    pub fn pos(&self) -> usize { self.bytes.len() }

    #[inline]
    fn emit_byte(&mut self, b: u8) { self.bytes.push(b); }
    #[inline]
    fn emit_i32(&mut self, v: i32) { self.bytes.extend_from_slice(&v.to_le_bytes()); }
    #[inline]
    fn emit_i64(&mut self, v: i64) { self.bytes.extend_from_slice(&v.to_le_bytes()); }

    #[inline]
    pub fn patch_i32(&mut self, pos: usize, v: i32) {
        self.bytes[pos..pos+4].copy_from_slice(&v.to_le_bytes());
    }

    // REX prefix - emit only when W=1 or any extension bit is needed
    #[inline]
    fn rex(&mut self, w: bool, r: Reg, b: Reg) {
        let byte = 0x40 | ((w as u8)<<3) | ((r.ext() as u8)<<2) | (b.ext() as u8);
        if byte != 0x40 { self.emit_byte(byte); }
    }

    #[inline]
    fn rex_w(&mut self, r: Reg, b: Reg) {
        self.rex(true, r, b);
    }

    #[inline]
    fn modrm_rr(&mut self, reg: Reg, rm: Reg) {
        self.emit_byte(0xC0 | (reg.enc()<<3) | rm.enc());
    }

    // ModRM + optional SIB + displacement for [base + offset]
    #[inline]
    fn modrm_mem_impl(&mut self, reg_enc: u8, base: Reg, offset: i32) {
        let m = if offset == 0 && base != Reg::Rbp && base != Reg::R13 { 0u8 }
                else if (-128..=127).contains(&offset)                 { 1u8 }
                else                                                   { 2u8 };

        self.emit_byte((m<<6) | (reg_enc<<3) | base.enc());

        if base == Reg::Rsp || base == Reg::R12 { self.emit_byte(0x24); } // SIB escape
        match m {
            1 => self.emit_byte(offset as i8 as u8),
            2 => self.emit_i32(offset), _ => {}
        }
    }

    // bswap reg (16-bit needs xchg instead since bswap r16 is undefined behavior on x86)
    #[inline]
    pub fn bswap(&mut self, r: Reg, size: u8) {
        match size {
            2 => {
                // xchg al, ah - swap the two bytes of the low word
                // ror r16, 8 is the clean way
                if r.ext() { self.bytes.push(0x41); }
                self.bytes.push(0x66); // operand size prefix
                self.bytes.push(0xC1); // ror r/m16, imm8
                self.bytes.push(0xC8 | r.enc());
                self.bytes.push(8);
            }
            4 => {
                if r.ext() { self.bytes.push(0x41); }
                self.bytes.push(0x0F);
                self.bytes.push(0xC8 | r.enc());
            }
            _ => { // 8
                self.bytes.push(if r.ext() { 0x49 } else { 0x48 });
                self.bytes.push(0x0F);
                self.bytes.push(0xC8 | r.enc());
            }
        }
    }

    // lzcnt r64, r64 (F3 REX.W 0F BD /r)
    #[inline]
    pub fn lzcnt(&mut self, dst: Reg, src: Reg) {
        self.bytes.push(0xF3);
        let rex = 0x48 | ((dst.ext() as u8) << 2) | (src.ext() as u8);
        self.bytes.push(rex);
        self.bytes.push(0x0F);
        self.bytes.push(0xBD);
        self.bytes.push(0xC0 | (dst.enc() << 3) | src.enc());
    }

    // tzcnt r64, r64 (F3 REX.W 0F BC /r)
    #[inline]
    pub fn tzcnt(&mut self, dst: Reg, src: Reg) {
        self.bytes.push(0xF3);
        let rex = 0x48 | ((dst.ext() as u8) << 2) | (src.ext() as u8);
        self.bytes.push(rex);
        self.bytes.push(0x0F);
        self.bytes.push(0xBC);
        self.bytes.push(0xC0 | (dst.enc() << 3) | src.enc());
    }

    // popcnt r64, r64 (F3 REX.W 0F B8 /r)
    #[inline]
    pub fn popcnt(&mut self, dst: Reg, src: Reg) {
        self.bytes.push(0xF3);
        let rex = 0x48 | ((dst.ext() as u8) << 2) | (src.ext() as u8);
        self.bytes.push(rex);
        self.bytes.push(0x0F);
        self.bytes.push(0xB8);
        self.bytes.push(0xC0 | (dst.enc() << 3) | src.enc());
    }

    // ud2 - guaranteed undefined instruction, causes SIGILL
    #[inline]
    pub fn ud2(&mut self) {
        self.bytes.push(0x0F);
        self.bytes.push(0x0B);
    }

    // ModRM + optional SIB + displacement for [base + offset]
    #[inline]
    fn modrm_mem(&mut self, reg: Reg, base: Reg, offset: i32) {
        self.modrm_mem_impl(reg.enc(), base, offset);
    }

    #[inline]
    pub fn cmp_rr(&mut self, lhs: Reg, rhs: Reg) {
        self.rex_w(rhs, lhs);
        self.emit_byte(0x39);
        self.modrm_rr(rhs, lhs);
    }

    #[inline]
    pub fn setcc(&mut self, dst: Reg, code: u8) {
        if dst.ext() { self.emit_byte(0x41); }
        self.emit_byte(0x0F);
        self.emit_byte(code);
        self.emit_byte(0xC0 | dst.enc());
    }

    #[inline]
    pub fn movzx_rr(&mut self, dst: Reg, src: Reg) {
        self.rex_w(dst, src);
        self.emit_byte(0x0F);
        self.emit_byte(0xB6);
        self.modrm_rr(dst, src);
    }

    #[inline]
    pub fn mov_rr(&mut self, dst: Reg, src: Reg) {
        if dst == src { return; }
        self.rex_w(src, dst);
        self.emit_byte(0x89);
        self.modrm_rr(src, dst);
    }

    #[inline]
    pub fn mov_ri64(&mut self, dst: Reg, imm: i64) {
        if imm == 0 { return self.xor_rr(dst, dst); }
        if (0..=0xFFFF_FFFF).contains(&imm) {
            if dst.ext() { self.emit_byte(0x41); }
            self.emit_byte(0xB8 | dst.enc());
            self.emit_i32(imm as i32);
        } else {
            self.rex_w(Reg::Rax, dst);
            self.emit_byte(0xB8 | dst.enc());
            self.emit_i64(imm);
        }
    }

    #[inline]
    pub fn mov_load (&mut self, dst: Reg, base: Reg, off: i32, is64: bool) {
        self.rex(is64, dst,  base); self.emit_byte(0x8B); self.modrm_mem(dst,  base, off);
    }
    // movsx r64, byte [base+off]
    #[inline]
    pub fn movsx8_load(&mut self, dst: Reg, base: Reg, off: i32) {
        self.rex_w(dst, base);
        self.bytes.extend_from_slice(&[0x0F, 0xBE]);
        self.modrm_mem(dst, base, off);
    }
    // movzx r64, byte [base+off]
    #[inline]
    pub fn movzx8_load(&mut self, dst: Reg, base: Reg, off: i32) {
        self.rex_w(dst, base);
        self.bytes.extend_from_slice(&[0x0F, 0xB6]);
        self.modrm_mem(dst, base, off);
    }
    // movsx r64, word [base+off]
    #[inline]
    pub fn movsx16_load(&mut self, dst: Reg, base: Reg, off: i32) {
        self.rex_w(dst, base);
        self.bytes.extend_from_slice(&[0x0F, 0xBF]);
        self.modrm_mem(dst, base, off);
    }
    // movzx r64, word [base+off]
    #[inline]
    pub fn movzx16_load(&mut self, dst: Reg, base: Reg, off: i32) {
        self.rex_w(dst, base);
        self.bytes.extend_from_slice(&[0x0F, 0xB7]);
        self.modrm_mem(dst, base, off);
    }
    // mov byte [base+off], src
    #[inline]
    pub fn mov_store8(&mut self, base: Reg, off: i32, src: Reg) {
        // need REX if src is sil/dil/spl/bpl (rsi/rdi/rsp/rbp low byte)
        if src.ext() || src as u8 >= 4 {
            self.emit_byte(0x40 | (src.ext() as u8));
        }
        self.emit_byte(0x88);
        self.modrm_mem(src, base, off);
    }
    // mov word [base+off], src
    #[inline]
    pub fn mov_store16(&mut self, base: Reg, off: i32, src: Reg) {
        self.emit_byte(0x66); // operand size prefix
        if src.ext() { self.emit_byte(0x41); }
        self.emit_byte(0x89);
        self.modrm_mem(src, base, off);
    }
    // movsx r64, r8
    #[inline]
    pub fn movsx_r8(&mut self, dst: Reg, src: Reg) {
        self.rex_w(dst, src);
        self.bytes.extend_from_slice(&[0x0F, 0xBE]);
        self.modrm_rr(dst, src);
    }
    // movsx r64, r16
    #[inline]
    pub fn movsx_r16(&mut self, dst: Reg, src: Reg) {
        self.rex_w(dst, src);
        self.bytes.extend_from_slice(&[0x0F, 0xBF]);
        self.modrm_rr(dst, src);
    }
    // movsxd r64, r32
    #[inline]
    pub fn movsxd(&mut self, dst: Reg, src: Reg) {
        self.rex_w(dst, src);
        self.emit_byte(0x63);
        self.modrm_rr(dst, src);
    }
    // movzx r64, r8
    #[inline]
    pub fn movzx_r8(&mut self, dst: Reg, src: Reg) {
        self.rex_w(dst, src);
        self.bytes.extend_from_slice(&[0x0F, 0xB6]);
        self.modrm_rr(dst, src);
    }
    // movzx r64, r16
    #[inline]
    pub fn movzx_r16(&mut self, dst: Reg, src: Reg) {
        self.rex_w(dst, src);
        self.bytes.extend_from_slice(&[0x0F, 0xB7]);
        self.modrm_rr(dst, src);
    }
    #[inline]
    pub fn mov_store(&mut self, base: Reg, off: i32, src: Reg, is64: bool) {
        self.rex(is64, src,  base); self.emit_byte(0x89); self.modrm_mem(src,  base, off);
    }
    #[inline]
    pub fn lea      (&mut self, dst: Reg, base: Reg, off: i32)             {
        self.rex_w(dst, base);      self.emit_byte(0x8D); self.modrm_mem(dst,  base, off);
    }

    #[inline]
    pub fn movq_xmm_load(&mut self, dst: XmmReg, base: Reg, off: i32) {
        // REX byte: 0100 W R X B
        // W=1 (64-bit), R=xmm>=8, B=base>=8
        let rex = 0x48 | ((dst as u8 >= 8) as u8) << 2 | ((base as u8 >= 8) as u8);
        self.emit_byte(0x66);
        self.emit_byte(rex);
        self.emit_byte(0x0F);
        self.emit_byte(0x6E);
        self.modrm_mem_impl(dst as u8 & 7, base, off);
    }
    #[inline]
    pub fn movq_xmm_store(&mut self, base: Reg, off: i32, src: XmmReg) {
        let rex = 0x48 | ((src as u8 >= 8) as u8) << 2 | ((base as u8 >= 8) as u8);
        self.emit_byte(0x66);
        self.emit_byte(rex);
        self.emit_byte(0x0F);
        self.emit_byte(0x7E);
        self.modrm_mem_impl(src as u8 & 7, base, off);
    }

    #[inline]
    pub fn neg_r(&mut self, r: Reg) {
        self.rex_w(Reg::Rax, r);
        self.emit_byte(0xF7);
        self.emit_byte(0xD8 | r.enc());
    }

    #[inline]
    pub fn test_rr(&mut self, r: Reg) {
        self.rex_w(r, r);
        self.emit_byte(0x85);
        self.modrm_rr(r, r);
    }

    #[inline]
    pub fn lea_rip(&mut self, dst: Reg) -> usize {
        self.rex_w(dst, Reg::Rax);  // REX.R set if dst >= R8
        self.emit_byte(0x8D);
        self.emit_byte(0x05 | (dst.enc() << 3));
        let patch = self.pos();
        self.emit_i32(0);
        patch
    }

    #[inline]
    pub fn add_ri8(&mut self, dst: Reg, imm: i8) {
        self.rex_w(Reg::Rax, dst);
        self.emit_byte(0x83);
        self.emit_byte(0xC0 | dst.enc());
        self.emit_byte(imm as u8);
    }

    #[inline]
    pub fn add_ri32(&mut self, dst: Reg, imm: i32) {
        self.rex_w(Reg::Rax, dst);
        self.emit_byte(0x81);
        self.emit_byte(0xC0 | dst.enc());
        self.emit_i32(imm);
    }
    #[inline]
    pub fn xor_rr   (&mut self, dst: Reg, src: Reg) {
        self.rex_w(src, dst); self.emit_byte(0x31); self.modrm_rr(src, dst);
    }
    #[inline]
    pub fn and_rr   (&mut self, dst: Reg, src: Reg) {
        self.rex_w(src, dst); self.emit_byte(0x21); self.modrm_rr(src, dst);
    }
    #[inline]
    pub fn or_rr    (&mut self, dst: Reg, src: Reg) {
        self.rex_w(src, dst); self.emit_byte(0x09); self.modrm_rr(src, dst);
    }
    #[inline]
    pub fn add_rr   (&mut self, dst: Reg, src: Reg) {
        self.rex_w(src, dst); self.emit_byte(0x01); self.modrm_rr(src, dst);
    }
    #[inline]
    pub fn sub_rr   (&mut self, dst: Reg, src: Reg) {
        self.rex_w(src, dst); self.emit_byte(0x29); self.modrm_rr(src, dst);
    }
    #[inline]
    pub fn sub_ri32(&mut self, dst: Reg, imm: i32) {
        self.rex_w(Reg::Rax, dst);
        self.emit_byte(0x81);
        self.emit_byte(0xE8 | dst.enc());
        self.emit_i32(imm);
    }
    #[inline]
    pub fn imul_ri(&mut self, dst: Reg, imm: i32) {
        self.rex_w(dst, dst);
        if (-128..=127).contains(&imm) {
            self.emit_byte(0x6B);
            self.modrm_rr(dst, dst);
            self.emit_byte(imm as i8 as u8);
        } else {
            self.emit_byte(0x69);
            self.modrm_rr(dst, dst);
            self.emit_i32(imm);
        }
    }
    #[inline]
    pub fn and_ri(&mut self, dst: Reg, imm: i32) {
        self.rex_w(Reg::Rax, dst);
        if (-128..=127).contains(&imm) {
            // 83 /4 ib - AND r/m64, imm8
            self.emit_byte(0x83);
            self.emit_byte(0xE0 | dst.enc());
            self.emit_byte(imm as i8 as u8);
        } else {
            // 81 /4 id - AND r/m64, imm32
            self.emit_byte(0x81);
            self.emit_byte(0xE0 | dst.enc());
            self.emit_i32(imm);
        }
    }
    #[inline]
    pub fn shl_ri(&mut self, dst: Reg, imm: u8) {
        self.rex_w(Reg::Rax, dst);
        self.emit_byte(0xC1);
        self.emit_byte(0xE0 | dst.enc());
        self.emit_byte(imm);
    }
    #[inline]
    pub fn imul_rr  (&mut self, dst: Reg, src: Reg) {
        self.rex_w(dst, src); self.bytes.extend_from_slice(&[0x0F, 0xAF]); self.modrm_rr(dst, src);
    }
    #[inline]
    pub fn cqo      (&mut self) { self.bytes.extend_from_slice(&[0x48, 0x99]); }

    #[inline]
    pub fn not_r(&mut self, r: Reg) {
        self.rex_w(Reg::Rax, r);
        self.emit_byte(0xF7);
        self.emit_byte(0xD0 | r.enc());
    }
    #[inline]
    pub fn idiv_r(&mut self, src: Reg) {
        self.rex_w(Reg::Rax, src); self.emit_byte(0xF7); self.emit_byte(0xF8 | src.enc());
    }

    #[inline]
    pub fn push_r(&mut self, r: Reg) {
        if r.ext() { self.emit_byte(0x41); } self.emit_byte(0x50 | r.enc());
    }
    #[inline]
    pub fn pop_r (&mut self, r: Reg) {
        if r.ext() { self.emit_byte(0x41); } self.emit_byte(0x58 | r.enc());
    }

    #[inline]
    pub fn sub_rsp(&mut self, v: i32) {
        self.bytes.extend_from_slice(&[0x48,0x81,0xEC]); self.emit_i32(v);
    }
    #[inline]
    pub fn add_rsp(&mut self, v: i32) {
        self.bytes.extend_from_slice(&[0x48,0x81,0xC4]); self.emit_i32(v);
    }

    #[inline]
    pub fn call_r(&mut self, r: Reg) {
        if r.ext() { self.emit_byte(0x41); }
        self.emit_byte(0xFF);
        self.emit_byte(0xD0 | r.enc());
    }
    #[inline]
    pub fn jmp_r(&mut self, r: Reg) {
        if r.ext() { self.emit_byte(0x41); }
        self.emit_byte(0xFF);
        self.emit_byte(0xE0 | r.enc());
    }

    /// JMP rel32
    #[inline]
    pub fn jmp_rel32(&mut self) -> usize {
        self.emit_byte(0xE9);
        let p = self.pos();
        self.emit_i32(-4);
        p
    }
    /// JMP rel8 (short jump, ±127 bytes)
    #[inline]
    pub fn jmp_rel8(&mut self) -> usize {
        self.emit_byte(0xEB);
        let p = self.pos();
        self.emit_byte(0);
        p
    }

    /// Patch a rel32 jump/call site (4 bytes)
    #[inline]
    pub fn patch_rel32(&mut self, patch: usize, target: usize) {
        let rel = (target as i64 - (patch as i64 + 4)) as i32;
        self.patch_i32(patch, rel);
    }
    /// Patch a rel8 jump site (1 byte)
    #[inline]
    pub fn patch_rel8(&mut self, patch: usize, target: usize) {
        let rel = (target as i64 - (patch as i64 + 1)) as i8;
        self.bytes[patch] = rel as u8;
    }

    // Conditional jumps - all rel32 variants (0x0F 0x8x)
    // Returns patch offset

    #[inline]
    pub fn je_rel32 (&mut self) -> usize {
        self.bytes.extend_from_slice(&[0x0F, 0x84]); let p = self.pos(); self.emit_i32(-4); p
    }
    #[inline]
    pub fn jne_rel32(&mut self) -> usize {
        self.bytes.extend_from_slice(&[0x0F, 0x85]); let p = self.pos(); self.emit_i32(-4); p
    }
    #[inline]
    pub fn jl_rel32 (&mut self) -> usize {
        self.bytes.extend_from_slice(&[0x0F, 0x8C]); let p = self.pos(); self.emit_i32(-4); p
    }
    #[inline]
    pub fn jle_rel32(&mut self) -> usize {
        self.bytes.extend_from_slice(&[0x0F, 0x8E]); let p = self.pos(); self.emit_i32(-4); p
    }
    #[inline]
    pub fn jg_rel32 (&mut self) -> usize {
        self.bytes.extend_from_slice(&[0x0F, 0x8F]); let p = self.pos(); self.emit_i32(-4); p
    }
    #[inline]
    pub fn jge_rel32(&mut self) -> usize {
        self.bytes.extend_from_slice(&[0x0F, 0x8D]); let p = self.pos(); self.emit_i32(-4); p
    }

    // Conditional jumps - rel8 variants (short, ±127 bytes)

    #[inline]
    pub fn je_rel8 (&mut self) -> usize {
        self.emit_byte(0x74); let p = self.pos(); self.emit_byte(0); p
    }
    #[inline]
    pub fn jne_rel8(&mut self) -> usize {
        self.emit_byte(0x75); let p = self.pos(); self.emit_byte(0); p
    }
    #[inline]
    pub fn jl_rel8 (&mut self) -> usize {
        self.emit_byte(0x7C); let p = self.pos(); self.emit_byte(0); p
    }
    #[inline]
    pub fn jle_rel8(&mut self) -> usize {
        self.emit_byte(0x7E); let p = self.pos(); self.emit_byte(0); p
    }
    #[inline]
    pub fn jg_rel8 (&mut self) -> usize {
        self.emit_byte(0x7F); let p = self.pos(); self.emit_byte(0); p
    }
    #[inline]
    pub fn jge_rel8(&mut self) -> usize {
        self.emit_byte(0x7D); let p = self.pos(); self.emit_byte(0); p
    }

    // Unsigned variants
    #[inline]
    pub fn jb_rel32 (&mut self) -> usize {
        self.bytes.extend_from_slice(&[0x0F, 0x82]); let p = self.pos(); self.emit_i32(-4); p
    }
    #[inline]
    pub fn jae_rel32(&mut self) -> usize {
        self.bytes.extend_from_slice(&[0x0F, 0x83]); let p = self.pos(); self.emit_i32(-4); p
    }
    #[inline]
    pub fn jbe_rel32(&mut self) -> usize {
        self.bytes.extend_from_slice(&[0x0F, 0x86]); let p = self.pos(); self.emit_i32(-4); p
    }
    #[inline]
    pub fn ja_rel32 (&mut self) -> usize {
        self.bytes.extend_from_slice(&[0x0F, 0x87]); let p = self.pos(); self.emit_i32(-4); p
    }

    #[inline]
    pub fn call_rel32(&mut self) -> usize {
        self.emit_byte(0xE8); let p = self.pos(); self.emit_i32(-4); p
    }

    #[inline]
    pub fn patch_call(&mut self, patch: usize, target: usize) {
        let rel = (target as i64 - (patch as i64 + 4)) as i32;
        self.patch_i32(patch, rel);
    }

    #[inline]
    pub fn ret(&mut self) { self.emit_byte(0xC3); }

    #[inline]
    fn xmm_rex(&mut self, dst: XmmReg, src: XmmReg) {
        let byte = 0x40 | ((dst as u8 >= 8) as u8) << 2 | ((src as u8 >= 8) as u8);
        if byte != 0x40 { self.emit_byte(byte); }
    }
    #[inline]
    fn xmm_modrm(&self, dst: XmmReg, src: XmmReg) -> u8 {
        0xC0 | (dst as u8 & 7) << 3 | (src as u8 & 7)
    }

    // Scalar double arithmetic
    #[inline]
    pub fn addsd(&mut self, dst: XmmReg, src: XmmReg) {
        self.xmm_rex(dst, src);
        self.bytes.extend_from_slice(&[0xF2, 0x0F, 0x58, self.xmm_modrm(dst, src)]);
    }
    #[inline]
    pub fn subsd(&mut self, dst: XmmReg, src: XmmReg) {
        self.xmm_rex(dst, src);
        self.bytes.extend_from_slice(&[0xF2, 0x0F, 0x5C, self.xmm_modrm(dst, src)]);
    }
    #[inline]
    pub fn mulsd(&mut self, dst: XmmReg, src: XmmReg) {
        self.xmm_rex(dst, src);
        self.bytes.extend_from_slice(&[0xF2, 0x0F, 0x59, self.xmm_modrm(dst, src)]);
    }
    #[inline]
    pub fn divsd(&mut self, dst: XmmReg, src: XmmReg) {
        self.xmm_rex(dst, src);
        self.bytes.extend_from_slice(&[0xF2, 0x0F, 0x5E, self.xmm_modrm(dst, src)]);
    }

    // Scalar float arithmetic
    #[inline]
    pub fn addss(&mut self, dst: XmmReg, src: XmmReg) {
        self.xmm_rex(dst, src);
        self.bytes.extend_from_slice(&[0xF3, 0x0F, 0x58, self.xmm_modrm(dst, src)]);
    }
    #[inline]
    pub fn subss(&mut self, dst: XmmReg, src: XmmReg) {
        self.xmm_rex(dst, src);
        self.bytes.extend_from_slice(&[0xF3, 0x0F, 0x5C, self.xmm_modrm(dst, src)]);
    }
    #[inline]
    pub fn mulss(&mut self, dst: XmmReg, src: XmmReg) {
        self.xmm_rex(dst, src);
        self.bytes.extend_from_slice(&[0xF3, 0x0F, 0x59, self.xmm_modrm(dst, src)]);
    }
    #[inline]
    pub fn divss(&mut self, dst: XmmReg, src: XmmReg) {
        self.xmm_rex(dst, src);
        self.bytes.extend_from_slice(&[0xF3, 0x0F, 0x5E, self.xmm_modrm(dst, src)]);
    }

    #[inline]
    pub fn ucomiss(&mut self, lhs: XmmReg, rhs: XmmReg) {
        self.xmm_rex(lhs, rhs);
        self.bytes.extend_from_slice(&[0x0F, 0x2E, self.xmm_modrm(lhs, rhs)]);
    }
    #[inline]
    pub fn ucomisd(&mut self, lhs: XmmReg, rhs: XmmReg) {
        self.xmm_rex(lhs, rhs);
        self.bytes.extend_from_slice(&[0x66, 0x0F, 0x2E, self.xmm_modrm(lhs, rhs)]);
    }

    #[inline]
    pub fn movss_rr(&mut self, dst: XmmReg, src: XmmReg) {
        if dst == src { return; }
        self.xmm_rex(dst, src);
        self.bytes.extend_from_slice(&[0xF3, 0x0F, 0x10, self.xmm_modrm(dst, src)]);
    }
    #[inline]
    pub fn movsd_rr(&mut self, dst: XmmReg, src: XmmReg) {
        if dst == src { return; }
        self.xmm_rex(dst, src);
        self.bytes.extend_from_slice(&[0xF2, 0x0F, 0x10, self.xmm_modrm(dst, src)]);
    }

    #[inline]
    pub fn cvtss2sd(&mut self, dst: XmmReg, src: XmmReg) {
        self.xmm_rex(dst, src);
        self.bytes.extend_from_slice(&[0xF3, 0x0F, 0x5A, self.xmm_modrm(dst, src)]);
    }
    #[inline]
    pub fn cvtsd2ss(&mut self, dst: XmmReg, src: XmmReg) {
        self.xmm_rex(dst, src);
        self.bytes.extend_from_slice(&[0xF2, 0x0F, 0x5A, self.xmm_modrm(dst, src)]);
    }
    #[inline]
    pub fn cvtsi2sd(&mut self, dst: XmmReg, src: Reg) {
        // REX.W=1 for 64-bit int src, REX.R if dst>=8, REX.B if src>=8
        let rex = 0x48 | ((dst as u8 >= 8) as u8) << 2 | (src.ext() as u8);
        self.bytes.extend_from_slice(&[0xF2, rex, 0x0F, 0x2A]);
        self.bytes.push(0xC0 | (dst as u8 & 7) << 3 | src.enc());
    }
    // cvtsi2ss: F3 0F 2A /r (int -> float)
    #[inline]
    pub fn cvtsi2ss(&mut self, dst: XmmReg, src: Reg) {
        let rex = 0x48 | ((dst as u8 >= 8) as u8) << 2 | (src.ext() as u8);
        self.bytes.extend_from_slice(&[0xF3, rex, 0x0F, 0x2A]);
        self.bytes.push(0xC0 | (dst as u8 & 7) << 3 | src.enc());
    }
    // cvttss2si: F3 0F 2C /r (float -> int, truncate)
    #[inline]
    pub fn cvttss2si(&mut self, dst: Reg, src: XmmReg) {
        let rex = 0x48 | (dst.ext() as u8) << 2 | ((src as u8 >= 8) as u8);
        self.bytes.extend_from_slice(&[0xF3, rex, 0x0F, 0x2C]);
        self.bytes.push(0xC0 | (dst.enc()) << 3 | (src as u8 & 7));
    }
    // cvttsd2si: F2 0F 2C /r (double -> int, truncate)
    #[inline]
    pub fn cvttsd2si(&mut self, dst: Reg, src: XmmReg) {
        let rex = 0x48 | (dst.ext() as u8) << 2 | ((src as u8 >= 8) as u8);
        self.bytes.extend_from_slice(&[0xF2, rex, 0x0F, 0x2C]);
        self.bytes.push(0xC0 | (dst.enc()) << 3 | (src as u8 & 7));
    }

    #[inline]
    pub fn movss_load_rip(&mut self, dst: XmmReg) -> usize {
        if dst as u8 >= 8 { self.emit_byte(0x44); }
        self.bytes.extend_from_slice(&[0xF3, 0x0F, 0x10]);
        self.bytes.push(0x05 | (dst as u8 & 7) << 3);
        let patch = self.pos(); self.emit_i32(0); patch
    }
    #[inline]
    pub fn movsd_load_rip(&mut self, dst: XmmReg) -> usize {
        if dst as u8 >= 8 { self.emit_byte(0x44); }
        self.bytes.extend_from_slice(&[0xF2, 0x0F, 0x10]);
        self.bytes.push(0x05 | (dst as u8 & 7) << 3);
        let patch = self.pos(); self.emit_i32(0); patch
    }
    #[inline]
    pub fn movsd_store_rip(&mut self, src: XmmReg) -> usize {
        if src as u8 >= 8 { self.emit_byte(0x44); }
        self.bytes.extend_from_slice(&[0xF2, 0x0F, 0x11]);
        self.bytes.push(0x05 | (src as u8 & 7) << 3);
        let patch = self.pos(); self.emit_i32(0); patch
    }

    #[inline]
    pub fn xorpd_rip(&mut self, dst: XmmReg) -> usize {
        if dst as u8 >= 8 { self.emit_byte(0x44); }
        self.bytes.extend_from_slice(&[0x66, 0x0F, 0x57]);
        self.bytes.push(0x05 | (dst as u8 & 7) << 3);
        let patch = self.pos(); self.emit_i32(0); patch
    }
    #[inline]
    pub fn xorps_rip(&mut self, dst: XmmReg) -> usize {
        if dst as u8 >= 8 { self.emit_byte(0x44); }
        self.bytes.extend_from_slice(&[0x0F, 0x57]);
        self.bytes.push(0x05 | (dst as u8 & 7) << 3);
        let patch = self.pos(); self.emit_i32(0); patch
    }

    #[inline]
    pub fn movss_load(&mut self, dst: XmmReg, base: Reg, off: i32) {
        if dst as u8 >= 8 { self.emit_byte(0x44); }
        self.bytes.extend_from_slice(&[0xF3, 0x0F, 0x10]);
        self.modrm_mem_impl(dst as u8 & 7, base, off);
    }
    #[inline]
    pub fn movss_store(&mut self, base: Reg, off: i32, src: XmmReg) {
        if src as u8 >= 8 { self.emit_byte(0x44); }
        self.bytes.extend_from_slice(&[0xF3, 0x0F, 0x11]);
        self.modrm_mem_impl(src as u8 & 7, base, off);
    }
    #[inline]
    pub fn movsd_load(&mut self, dst: XmmReg, base: Reg, off: i32) {
        if dst as u8 >= 8 { self.emit_byte(0x44); }
        self.bytes.extend_from_slice(&[0xF2, 0x0F, 0x10]);
        self.modrm_mem_impl(dst as u8 & 7, base, off);
    }
    #[inline]
    pub fn movsd_store(&mut self, base: Reg, off: i32, src: XmmReg) {
        if src as u8 >= 8 { self.emit_byte(0x44); }
        self.bytes.extend_from_slice(&[0xF2, 0x0F, 0x11]);
        self.modrm_mem_impl(src as u8 & 7, base, off);
    }
}

const VALUE_STACK_CAP: usize = 64;

pub struct ValueStack { vals: [CValue; VALUE_STACK_CAP], top: usize }

impl ValueStack {
    pub fn new() -> Self { Self { vals: [CValue::imm(TYPE_INT, 0); VALUE_STACK_CAP], top: 0 } }
    pub fn push(&mut self, v: CValue) { self.vals[self.top] = v; self.top += 1; }
    #[track_caller]
    pub fn pop (&mut self) -> CValue  { self.top -= 1; self.vals[self.top] }
    pub fn peek(&self)     -> CValue  { self.vals[self.top - 1] }
    pub fn len (&self)     -> usize   { self.top }
}

const MAX_LOCALS: usize = 128;

#[derive(Clone, Copy)]
pub struct LocalEntry {
    pub ty: TypeRef,     // 4
    pub rbp_off: i32,    // 4
    pub is_indirect: b32 // 4 - rbp_off holds a pointer to the value, not the value itself.
}

pub struct LocalTable {
    frame_bytes: i32,

    locals:      SmallVec<[LocalEntry; MAX_LOCALS]>,
    index:       IntMap<u64, u32>,              // hash -> index of most recent in current scope

    scope_stack: Vec<Vec<(u64, Option<u32>)>>,  // stack of (hash, previous_index) per scope
}

impl LocalTable {
    #[inline]
    pub fn new() -> Self {
        Self {
            locals: SmallVec::new(),
            index: IntMap::default(),
            scope_stack: vec![Default::default()],
            frame_bytes: 0
        }
    }

    #[inline]
    pub fn fix_last_ty(&mut self, ty: TypeRef) {
        if let Some(last) = self.locals.last_mut() {
            last.ty = ty;
        }
    }

    #[inline]
    fn alloc_impl(&mut self, hash: u64, ty: TypeRef, size: i32, is_indirect: bool) -> i32 {
        self.frame_bytes += size;
        let rbp_off = -(self.frame_bytes);

        let idx = self.locals.len() as u32;
        self.locals.push(LocalEntry { ty, rbp_off, is_indirect });

        if hash != 0 {
            // Save previous index for this hash so we can restore on scope exit
            let prev = self.index.insert(hash, idx);
            if let Some(scope) = self.scope_stack.last_mut() {
                scope.push((hash, prev));
            }
        }

        rbp_off
    }

    #[inline]
    pub fn alloc(&mut self, hash: u64, ty: TypeRef, type_table: &TypeTable) -> i32 {
        self.alloc_impl(hash, ty, type_table.size_of(ty) as _, false)
    }

    #[inline]
    pub fn alloc_vla(&mut self, hash: u64, vla_ty: TypeRef) -> i32 {
        self.alloc_impl(hash, vla_ty, 8, false)
    }

    #[inline]
    pub fn alloc_indirect(&mut self, hash: u64, ty: TypeRef) -> i32 {
        self.alloc_impl(hash, ty, 8, true)
    }

    #[inline]
    pub fn find(&self, hash: u64) -> Option<LocalEntry> {
        self.index.get(&hash).map(|&i| self.locals[i as usize])
    }

    #[inline]
    pub fn push_scope(&mut self) {
        self.scope_stack.push(Default::default());
    }

    #[inline]
    pub fn pop_scope(&mut self) {
        let Some(scope) = self.scope_stack.pop() else { return; };

        //
        // Restore previous index entries in reverse order
        //
        for (hash, prev) in scope.into_iter().rev() {
            match prev {
                Some(i) => { self.index.insert(hash, i); }
                None    => { self.index.remove(&hash); }
            }
        }

        // Frame space is permanent - don't shrink locals..
    }
}

bitflags::bitflags! {
    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
    pub struct SymFlags: u8 {
        const EXTERN = 0x1;
        const STATIC = 0x2;
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Symbol {
    pub code_off: u32,     // 4
    pub code_len: u32,     // 4

    // For procedures
    pub func_ty:  TypeRef, // 4

    pub flags:    SymFlags // 1
}

pub struct SymTable {
    index:        IntMap<u64, u32>,
    pub syms:     SmallVec<[Symbol; 64]>,

    pub name_buf:  Vec<u8>,
    pub name_offs: Vec<u32>,  // index -> offset into name_buf
    pub name_lens: Vec<u16>,  // index -> length
}

impl Deref for SymTable {
    type Target = SmallVec<[Symbol; 64]>;
    #[inline]
    fn deref(&self) -> &Self::Target { &self.syms }
}

impl DerefMut for SymTable {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target { &mut self.syms }
}

impl SymTable {
    #[inline]
    pub fn new() -> Self {
        Self {
            syms: SmallVec::new(),
            index: IntMap::with_capacity_and_hasher(128, Default::default()),
            name_offs: Vec::with_capacity(128),
            name_lens: Vec::with_capacity(128),
            name_buf: Vec::with_capacity(4096),
        }
    }

    #[inline]
    pub fn find(&self, hash: u64) -> Option<usize> {
        self.index.get(&hash).map(|&i| i as usize)
    }

    #[inline]
    pub fn patch_forward_declaration(
        &mut self,
        sym_index: usize,

        code_off:  u32,
        code_len:  u32,
        flags:     SymFlags
    ) {
        let sym = &mut self.syms[sym_index];
        sym.code_off = code_off;
        sym.code_len = code_len;
        sym.flags = flags;
    }

    #[inline]
    pub fn insert(
        &mut self,
        name: &str,
        code_off: u32, code_len: u32,
        flags: SymFlags,
        func_ty: Option<TypeRef>
    ) -> usize {
        let hash = hash_ident(name);
        if let Some(&i) = self.index.get(&hash) {
            let s = &mut self.syms[i as usize];
            s.code_off = code_off;
            s.flags = flags;
            return i as usize;
        }

        let name_off = self.name_buf.len() as u32;
        self.name_buf.extend_from_slice(name.as_bytes());

        let i = self.syms.len();
        self.syms.push(Symbol {
            code_off, code_len,
            func_ty: func_ty.unwrap_or(TYPE_VOID),
            flags,
        });
        self.name_offs.push(name_off);
        self.name_lens.push(name.len() as _);
        self.index.insert(hash, i as u32);

        i
    }

    #[inline]
    pub fn s(&self, i: usize) -> &str {
        let name_off = self.name_offs[i] as usize;
        let name_len = self.name_lens[i] as usize;

        unsafe {
            std::str::from_utf8_unchecked(
                &self.name_buf[name_off..name_off + name_len]
            )
        }
    }
}

#[derive(Clone, Copy)]
pub struct CallReloc {
    pub offset: u32,
    pub sym_index: u32,
    pub addend: i64
}

pub struct RodataReloc {
    pub text_off: u32,
    pub rodata_off: u32
}

pub struct DataReloc {
    pub text_off: u32,
    pub data_off: u32,
    pub is_bss:   b32
}

#[derive(Default)]
pub struct LoopContext {
    break_patches:    Vec<u32>,
    continue_patches: Vec<u32>,
}

#[derive(Copy, Clone)]
pub struct GlobalEntry {
    pub ty:       TypeRef, // 4

    pub data_off: u32,     // 4 - Offset into .data OR .bss
    pub name_off: u32,     // 4
    pub name_len: u16,     // 2

    // @BitFlagsCandidate
    pub is_bss:    b8,
    pub is_static: b8,
}

impl GlobalEntry {
    #[inline]
    pub fn s<'a>(&self, buf: &'a [u8]) -> &'a str {
        unsafe {
            std::str::from_utf8_unchecked(
                &buf[
                    self.name_off as usize
                    ..
                    self.name_off as usize + self.name_len as usize
                ]
            )
        }
    }
}

pub struct GlobalTable {
    index:        IntMap<u64, u32>,
    pub vars:     SmallVec<[GlobalEntry; 64]>,

    pub name_buf: Vec<u8>,

}

impl GlobalTable {
    #[inline]
    pub fn new() -> Self {
        Self {
            vars: SmallVec::new(),
            index: IntMap::default(),
            name_buf: Vec::new()
        }
    }

    #[inline]
    pub fn find(&self, hash: u64) -> Option<GlobalEntry> {
        self.index.get(&hash).map(|&i| self.vars[i as usize])
    }

    #[inline]
    pub fn insert(&mut self, name: &str, hash: u64, ty: TypeRef, data_off: u32, is_bss: bool, is_static: bool) {
        let name_off = self.name_buf.len() as u32;
        self.name_buf.extend_from_slice(name.as_bytes());
        let i = self.vars.len() as u32;
        self.vars.push(GlobalEntry {
            name_off, name_len: name.len() as u16,
            ty, data_off,
            is_bss, is_static
        });
        self.index.insert(hash, i);
    }
}

pub struct Compiler {
    pub code:          CodeBuf,
    pub xmms:          XmmAlloc,
    pub regs:          RegAlloc,
    pub vstack:        ValueStack, // Huge struct (~16 cache lines)

    pub in_global_context:                             bx,   // @KindaHack, used in compile_type
    pub dont_decay_types_of_array_globals_to_pointers: bx,   // @KindaHack, used in sizeof

    pub ret_ty:        TypeRef,
    pub ret_ptr_off:   Option<i32>,

    pub types:         TypeTable,
    pub typedefs:      IntMap<u64, TypeRef>,
    pub tags:          IntMap<u64, TypeRef>,
    pub enum_consts:   IntMap<u64, i64>,

    pub data:          Vec<u8>,
    pub data_relocs:   Vec<DataReloc>,
    pub bss_size:      usize,

    pub loop_stack:    Vec<LoopContext>,

    pub call_relocs:   Vec<CallReloc>,
    pub rodata:        Vec<u8>,
    pub rodata_relocs: Vec<RodataReloc>,
    pub rodata_interner: IntMap<RodataHash, u32>, // Bytes hash -> offset into rodata

    pub vla_sizes:     IntMap<TypeRef, i32>,  // Fresh TypeRef -> rbp_off of total byte size local

    // Reset per function
    pub locals:        LocalTable,  // Huge struct (~50 cache lines)
    pub globals:       GlobalTable, // Huge struct (~25 cache lines)

    pub syms:          SymTable,    // Huge struct (~33 cache lines)

    pub pp:            PP,

    pub scratch_buffer_for_unescaping_string_literals_to_further_intern_them_into_rodata: SmallVec<[u8; 2048]>,
}

impl Deref for Compiler {
    type Target = PP;

    #[inline]
    fn deref(&self) -> &PP { &self.pp }
}

impl DerefMut for Compiler {
    #[inline]
    fn deref_mut(&mut self) -> &mut PP { &mut self.pp }
}

#[allow(unused)]
impl Compiler {  // TypeTable helpers
    #[inline] fn ty(&self, id: TypeRef) -> &TypeEntry { self.types.get(id) }
    #[inline] fn is_float(&self, id: TypeRef) -> bool { self.types.get(id).is_float() }
    #[inline] fn is_integer(&self, id: TypeRef) -> bool { self.types.get(id).is_integer() }
    #[inline] fn is_ptr(&self, id: TypeRef) -> bool { self.types.get(id).is_ptr() }
    #[inline] fn is_array(&self, id: TypeRef) -> bool { self.types.get(id).is_array() }
    #[inline] fn size_of(&self, id: TypeRef) -> u32 { self.types.size_of(id) }
    #[inline] fn align_of(&self, id: TypeRef) -> u32 { self.types.align_of(id) }
    #[inline] fn is64(&self, id: TypeRef) -> bool { self.types.size_of(id) == 8 }
    #[inline] fn get(&self, id: TypeRef) -> &TypeEntry { self.types.get(id) }
    #[inline] fn get_kind(&self, id: TypeRef) -> TypeKind { self.types.get_kind(id) }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum SysVClass { Integer, Sse, Memory }

impl SysVClass {
    #[inline]
    fn merge(self, other: Self) -> Self {
        let a = self;
        let b = other;

             if a == b                                             { a }
        else if a == SysVClass::Memory  || b == SysVClass::Memory  { SysVClass::Memory }
        else if a == SysVClass::Integer || b == SysVClass::Integer { SysVClass::Integer }
        else                                                       { SysVClass::Sse }
    }
}

impl Compiler {
    #[inline]
    fn classify_eightbyte(&self, ty: TypeRef, offset: u32, end: u32) -> SysVClass {
        match self.get_kind(ty) {
            TypeKind::Struct => {
                let e = self.types.get(ty);
                let fields = self.types.field_slice(e.field_start(), e.field_count());

                let mut cls: Option<SysVClass> = None;
                for f in fields {
                    let f_start = f.offset;
                    let f_end   = f_start + self.types.size_of(f.ty);

                    //
                    // Skip fields not overlapping this eightbyte
                    //
                    if f_end <= offset || f_start >= end { continue }

                    let f_cls = self.classify_eightbyte(
                        f.ty,
                        offset.saturating_sub(f_start),
                        (end - f_start).min(self.types.size_of(f.ty))
                    );
                    cls = Some(match cls {
                        None    => f_cls,
                        Some(c) => c.merge(f_cls),
                    });
                }

                cls.unwrap_or(SysVClass::Integer)
            }

            TypeKind::Float | TypeKind::Double => SysVClass::Sse,

            TypeKind::Array => {
                let elem_ty  = self.types.get(ty).elem();
                let elem_sz  = self.types.size_of(elem_ty);
                if elem_sz == 0 { return SysVClass::Integer; }

                self.classify_eightbyte(elem_ty, offset % elem_sz, elem_sz)
            }

            _ => SysVClass::Integer,
        }
    }

    #[inline]
    fn classify_struct(&self, ty: TypeRef) -> SmallVec<[SysVClass; 4]> {
        let size = self.types.size_of(ty);

        //
        // > 16 bytes always goes to memory (SYSV)
        //
        if size > 16 {
            return smallvec![SysVClass::Memory];
        }

        let n_eightbytes = (size + 7) / 8;
        let mut classes = SmallVec::new();

        for i in 0..n_eightbytes {
            let offset = i * 8;
            let end    = (offset + 8).min(size);
            classes.push(self.classify_eightbyte(ty, offset, end));
        }

        classes
    }
}

impl Compiler {
    #[inline]
    pub fn new(pp: PP) -> Self {
        let mut c = Self {
            pp,
            ret_ptr_off: None,
            in_global_context: true,
            tags: Default::default(),
            loop_stack: Vec::new(),
            types: TypeTable::new(),
            rodata_interner: Default::default(),
            enum_consts: Default::default(),
            typedefs: Default::default(),
            vla_sizes: Default::default(),
            data: Vec::new(), globals: GlobalTable::new(),
            bss_size: 0, data_relocs: Vec::new(),
            code: CodeBuf::new(), vstack: ValueStack::new(),
            regs: RegAlloc::new(), xmms: XmmAlloc::new(),
            syms: SymTable::new(), call_relocs: Vec::new(),
            rodata: Vec::new(), rodata_relocs: Vec::new(),
            locals: LocalTable::new(), ret_ty: TYPE_VOID,
            dont_decay_types_of_array_globals_to_pointers: false,
            scratch_buffer_for_unescaping_string_literals_to_further_intern_them_into_rodata: Default::default()
        };

        // @Incomplete
        // @Incomplete
        // @Incomplete
        let void_ptr = c.types.ptr_to(TYPE_VOID);
        c.typedefs.insert(hash_ident("__builtin_va_list"), void_ptr);
        c.typedefs.insert(hash_ident("__gnuc_va_list"), void_ptr);
        c.typedefs.insert(hash_ident("va_list"), void_ptr);

        c
    }

    // cur/peek/next come from Deref to PP.
    // eat() - assert kind and advance, or error
    #[inline]
    fn expect(&mut self, kind: TK, what: &'static str) -> CResult<Token> {
        let t = self.current_token;
        if t.kind == kind {
            self.next(); Ok(t)
        } else {
            Err(CError::Expected {
                span: t.span, expected: what, got: self.s(t).to_owned()
            })
        }
    }

    #[inline]
    fn eat_ident(&mut self, what: &'static str) -> CResult<Token> {
        self.expect(TK::Ident, what)
    }

    #[inline]
    fn at_eof(&self) -> bool { self.current_token.kind == TK::Eof }

    #[inline]
    fn unescape_len(s: &str) -> usize {
        let bytes = s.as_bytes();
        let mut len = 0usize;
        let mut i   = 0usize;
        while i < bytes.len() {
            if bytes[i] == b'\\' && i + 1 < bytes.len() { i += 1; }
            len += 1;
            i   += 1;
        }
        len
    }

    #[inline]
    fn rodata_intern(&mut self, bytes: &[u8]) -> u32 {
        let hash = hash_bytes_for_rodata_interning(bytes);
        *self.rodata_interner.entry(hash).or_insert_with(|| {
            let off = self.rodata.len() as u32;
            self.rodata.extend_from_slice(bytes);
            if bytes.last() != Some(&0) { self.rodata.push(0); }
            off
        })
    }

    #[inline]
    fn parse_expr_and_get_its_type(&mut self) -> CResult<TypeRef> {
        self.with_rollback(|c| {
            c.dont_decay_types_of_array_globals_to_pointers = true;
            c.compile_expr()?;
            c.dont_decay_types_of_array_globals_to_pointers = false;
            Ok(c.vstack.pop().ty)
        })
    }

    #[inline]
    fn compile_array_dim_loop(&mut self, mut ty: TypeRef) -> CResult<TypeRef> {
        while self.current_token.kind == TK::LSquare {
            self.next(); // [

            if self.current_token.kind == TK::RSquare {
                self.next();

                ty = self.types.array_of(ty, 0);
            } else if self.current_token.kind == TK::Number || !self.is_hash_a_local_or_a_global(self.current_token.hash) {
                let len = self.try_parse_and_eval_const_int()? as u32;
                self.expect(TK::RSquare, "']'")?;

                ty = self.types.array_of(ty, len);
            } else {
                if self.in_global_context {
                    return Err(CError::VlaInGlobalContext { span: self.current_token.span });
                }

                self.compile_expr()?;
                self.expect(TK::RSquare, "']'")?;

                let (r, _) = self.pop_reg()?;
                let dim_off = self.locals.alloc_vla(HASH_HIDDEN_LOCAL, TYPE_LONG);
                self.code.mov_store(Reg::Rbp, dim_off, r, true);
                self.regs.free(r);

                ty = self.types.alloc_fresh(
                    TypeKind::Array, QualFlags::empty(), TypeFlags::empty(),
                    ty, 0, dim_off as u32
                );
            }
        }

        Ok(ty)
    }

    #[inline]
    fn compile_struct(&mut self) -> CResult<TypeRef> {
        self.compile_struct_or_union(false)
    }

    #[inline]
    fn compile_union(&mut self) -> CResult<TypeRef> {
        self.compile_struct_or_union(true)
    }

    fn compile_struct_or_union(&mut self, is_union: bool) -> CResult<TypeRef> {
        if [HASH_UNION, HASH_STRUCT].contains(&self.current_token.hash) {
            self.next();
        }

        let tag = (self.current_token.kind == TK::Ident).then(|| self.next());

        if let Some(tag) = tag && let Some(&existing_ty) = self.tags.get(&tag.hash) {
            //
            // If already fully defined, return it
            // If it's an incomplete forward decl and we have a body, fall through
            //
            let is_incomplete =
                self.types.get(existing_ty).flags == TypeFlags::FORWARD_DECLARATION;

            if !is_incomplete || self.current_token.kind != TK::LCurly {
                return Ok(existing_ty);
            }

            // ... Fall through to parse the full definition
        }

        if self.current_token.kind != TK::LCurly {
            if let Some(tag) = tag {
                //
                // Forward declaration - register incomplete type if not already known
                //

                if !self.tags.contains_key(&tag.hash) {
                    //
                    // Empty placeholder
                    //

                    let ty = self.types.make_struct_with_flags(
                        0, 0,
                        TypeFlags::FORWARD_DECLARATION
                    );
                    self.tags.insert(tag.hash, ty);
                }

                return Ok(*self.tags.get(&tag.hash).unwrap());
            }

            let t = tag.unwrap_or(self.current_token);

            return Err(CError::UnknownType {
                name: t.s(&self.src_arena).to_owned(),
                span: t.span
            });
        }

        self.next(); // {

        let mut field_spans   = SmallVec::<[Span;       4]>::new();
        let mut field_names   = SmallVec::<[u64;        4]>::new();
        let mut field_entries = SmallVec::<[FieldEntry; 4]>::new();

        loop {
            let base_ty = match self.current_token.kind {
                TK::RCurly => break,

                TK::Comma => {
                    self.next();
                    field_entries.last().unwrap().ty  // @Incomplete
                }

                TK::SemiColon => {
                    self.next();
                    continue;
                }

                _ => self.compile_base_type()?
            };

            let (ty, name_tok) = self.parse_declarator(base_ty)?;
            let name = name_tok.ok_or_else(|| CError::Expected {
                span: self.current_token.span,
                expected: "field name",
                got: self.s(self.current_token).to_owned(),
            })?;

            if self.current_token.kind == TK::Colon {
                // @Incomplete
                //
                // int _flags2:24;
                //

                self.next();
                self.expect(TK::Number, "bit-width")?;
            }

            let e = FieldEntry {
                ty,

                offset: 0,   // Set during struct layouting

                // @Incomplete
                bit_offset: 0,
                bit_width:  0,

                _pad: 0
            };

            field_entries.push(e);
            field_names.push(name.hash);
            field_spans.push(name.span);
        }

        self.expect(TK::RCurly, "'}'")?;

        let (start, count) = self.types.alloc_fields(&field_entries, &field_names, &field_spans);

        // @Speed: Cache layouts?
        let ty = if let Some(tag) = tag && let Some(&existing_ty) = self.tags.get(&tag.hash)
            && self.types.get(existing_ty).flags == TypeFlags::FORWARD_DECLARATION
        {
            //
            // Patch the forward declaration in-place
            //

            if is_union {
                self.types.layout_union(start, count);
            } else {
                self.types.layout_struct(start, count);
            }
            self.types.patch_struct_forward_decl(existing_ty, start, count, is_union);

            existing_ty  // reuse the same TypeRef
        } else {
            if is_union {
                self.types.layout_union(start, count);
                self.types.make_union(start, count)
            } else {
                self.types.layout_struct(start, count);
                self.types.make_struct(start, count)
            }
        };

        if let Some(tag) = tag { self.tags.insert(tag.hash, ty); }

        Ok(ty)
    }

    fn compile_enum(&mut self) -> CResult<TypeRef> {
        let tag = (
            self.current_token.kind == TK::Ident
                && !self.can_hash_start_a_type(self.current_token.hash)
        ).then(|| self.next());

        //
        // Return existing if already defined and no body
        //
        if let Some(tag) = tag && let Some(&existing_ty) = self.tags.get(&tag.hash) {
            //
            // If already fully defined, return it
            // If it's an incomplete forward decl and we have a body, fall through
            //
            let is_incomplete =
                self.types.get(existing_ty).flags == TypeFlags::FORWARD_DECLARATION;

            if !is_incomplete || self.current_token.kind != TK::LCurly {
                return Ok(existing_ty);
            }

            // ... Fall through to parse the full definition
        }

        if self.current_token.kind != TK::LCurly {
            if let Some(tag) = tag {
                //
                // Forward declaration - register incomplete type if not already known
                //

                if !self.tags.contains_key(&tag.hash) {
                    //
                    // Empty placeholder
                    //

                    let ty = self.types.intern(
                        TypeKind::Enum,
                        QualFlags::empty(),
                        TypeFlags::FORWARD_DECLARATION,
                        TypeRef(0), 0, 0
                    );
                    self.tags.insert(tag.hash, ty);
                }

                return Ok(*self.tags.get(&tag.hash).unwrap());
            }

            let t = tag.unwrap_or(self.current_token);
            return Err(CError::UnknownType {
                name: t.s(&self.src_arena).to_owned(),
                span: t.span,
            });
        }

        self.next(); // {

        let mut next_val: i64 = 0;
        while self.current_token.kind != TK::RCurly && !self.at_eof() {
            let name_tok = self.eat_ident("enumerator name")?;

            let val = if self.current_token.kind == TK::Eq {
                self.next(); // =
                let v = self.try_parse_and_eval_const_int()?;
                next_val = v;
                v
            } else {
                next_val
            };

            self.enum_consts.insert(name_tok.hash, val);
            next_val += 1;

            if self.current_token.kind == TK::Comma { self.next(); }
        }

        self.expect(TK::RCurly, "'}'")?;

        let ty = if let Some(tag) = tag && let Some(&existing_ty) = self.tags.get(&tag.hash)
            && self.types.get(existing_ty).flags == TypeFlags::FORWARD_DECLARATION
        {
            //
            // Patch the forward declaration in-place
            //

            self.types.patch_enum_forward_decl(existing_ty);

            existing_ty  // reuse the same TypeRef
        } else {
            self.types.intern(
                TypeKind::Enum, QualFlags::empty(), TypeFlags::empty(),
                TypeRef(0), 0, 0
            )
        };

        if let Some(tag) = tag { self.tags.insert(tag.hash, ty); }

        Ok(ty)
    }

    #[inline]
    fn compile_type(&mut self) -> CResult<TypeRef> {
        let base = self.compile_base_type()?;
        let (ty, _) = self.parse_declarator(base)?;
        Ok(ty)
    }

    fn compile_base_type(&mut self) -> CResult<TypeRef> {
        let mut quals = QualFlags::empty();
        let mut ty = None;

        //
        // Consume qualifiers and type keywords in any order.... Sigh.......
        //

        loop {
            let t = self.current_token;
            if t.kind != TK::Ident { break; }
            match t.hash {
                HASH_BOOL | HASH__BOOL => { if ty.is_none() { ty = Some(TYPE_BOOL); } self.next(); }
                HASH_INT    => { if ty.is_none() { ty = Some(TYPE_INT);    } self.next(); }
                HASH_CHAR   => { if ty.is_none() { ty = Some(TYPE_CHAR);   } self.next(); }
                HASH_SHORT  => { if ty.is_none() { ty = Some(TYPE_SHORT);  } self.next(); }
                HASH_VOID   => { if ty.is_none() { ty = Some(TYPE_VOID);   } self.next(); }
                HASH_FLOAT  => { if ty.is_none() { ty = Some(TYPE_FLOAT);  } self.next(); }
                HASH_DOUBLE => { if ty.is_none() { ty = Some(TYPE_DOUBLE); } self.next(); }
                HASH_LONG => {
                    self.next();
                    ty = Some(match self.current_token.hash {
                        HASH_LONG   => { self.next(); TYPE_LLONG   }  // long long
                        HASH_INT    => { self.next(); TYPE_LONG    }  // long int (explicit)
                        HASH_DOUBLE => { self.next(); TYPE_LDOUBLE }  // long double
                        _           =>                TYPE_LONG       // bare long
                    });
                }
                HASH_CONST    => { quals |= QualFlags::CONST;    self.next(); }
                HASH_VOLATILE => { quals |= QualFlags::VOLATILE; self.next(); }
                HASH_RESTRICT => { quals |= QualFlags::RESTRICT; self.next(); }
                HASH_UNSIGNED => { quals |= QualFlags::UNSIGNED; self.next(); }
                HASH_SIGNED   => { self.next(); } // default, ignore
                HASH_REGISTER => { self.next(); } // hint only, no-op
                HASH_AUTO     => { self.next(); } // default storage, no-op
                HASH_INLINE   => { self.next(); } // handled in compile_top_level
                HASH_STATIC   => { self.next(); } // handled in compile_top_level

                HASH_TYPEOF => {
                    self.next(); // typeof

                    let mut has_parens = false;
                    if self.current_token.kind == TK::LParen { self.next(); has_parens = true }

                    ty = Some(self.parse_expr_and_get_its_type()?);

                    if has_parens { self.expect(TK::RParen, "')'")?; }
                }

                HASH_STRUCT => {
                    self.next(); // struct
                    ty = Some(self.compile_struct()?);
                }

                HASH_UNION => {
                    self.next(); // union
                    ty = Some(self.compile_union()?);
                }

                HASH_ENUM => {
                    self.next(); // enum
                    ty = Some(self.compile_enum()?);
                }

                //
                // Only try typedef lookup if we haven't seen a base type yet,
                // otherwise this is just the next token after the type.
                //
                _ => if ty.is_none() {
                    if let Some(&typedef) = self.typedefs.get(&t.hash) {
                        self.next();
                        ty = Some(typedef);
                    } else {
                        return Err(CError::UnknownType {
                            span: t.span,
                            name: t.s(&self.pp.src_arena).to_owned(),
                        });
                    }
                } else {
                    break;
                }
            }
        }

        let ty = ty.unwrap_or(TYPE_INT); // Implicit int..................... Sigh...........
        Ok(if quals.is_empty() {
            ty
        } else if is_type_builtin(ty) && quals.difference(QualFlags::UNSIGNED).is_empty() {
            unsign_a_builtin_type(ty)
        } else {
            self.types.qualify(ty, quals)
        })
    }

    /// Parse a declarator on top of `base_ty`.
    /// Returns (final_type, name_token).
    fn parse_declarator(&mut self, base_ty: TypeRef) -> CResult<(TypeRef, Option<Token>)> {
        self.parse_declarator_impl(base_ty).map(|(ty, name, _)| (ty, name))
    }

    /// Parse a declarator on top of `base_ty`.
    /// Returns (final_type, name_token, params).
    fn parse_declarator_impl(
        &mut self,
        base_ty: TypeRef
    ) -> CResult<(TypeRef, Option<Token>, Option<Vec<(TypeRef, u64, Span)>>)> {
        //
        // Collect leading pointer chain
        //
        let mut ptr_chain = SmallVec::<[_; 8]>::new();
        while self.current_token.kind == TK::Star {
            self.next(); // *

            let mut ptr_quals = QualFlags::empty();
            loop {
                match self.current_token.hash {
                    HASH_CONST    => { ptr_quals |= QualFlags::CONST;    self.next(); }
                    HASH_VOLATILE => { ptr_quals |= QualFlags::VOLATILE; self.next(); }
                    HASH_RESTRICT => { ptr_quals |= QualFlags::RESTRICT; self.next(); }
                    _             => break,
                }
            }

            ptr_chain.push(ptr_quals);
        }

        //
        // Grouped declarator: (*name), (**name), (*name)(params) etc.
        // Detect '(' not followed by a type.
        //
        // Sigh.............................
        //
        let grouped = self.current_token.kind == TK::LParen
            && !self.can_hash_start_a_type(self.next_token.hash)
            && self.next_token.kind != TK::RParen  // () is empty params, not grouping
            && (self.next_token.kind == TK::Star
                || self.next_token.kind == TK::LParen
                || (self.next_token.kind == TK::Ident
                    && !self.can_hash_start_a_type(self.next_token.hash)));

        let (inner, name_tok) = if grouped {
            self.next(); // (

            //
            // Parse inner declarator with TYPE_VOID as placeholder base
            //
            let result = self.parse_declarator(TYPE_VOID)?;
            self.expect(TK::RParen, "')'")?;

            (Some(result.0), result.1)
        } else {
            //
            // Direct name or anonymous
            //
            let name = if self.current_token.kind == TK::Ident
                && !HASHES_THAT_START_TYPES.contains(&self.current_token.hash)
            {
                Some(self.next())
            } else {
                None
            };

            (None, name)
        };

        //
        // Before building the suffix, apply the pointer chain to base_ty
        // so that `void *` becomes the return type, not a wrapper around the func type.
        //
        let mut pointed_base = base_ty;
        for &ptr_quals in ptr_chain.iter().rev() {
            pointed_base = self.types.intern(
                TypeKind::Ptr,
                ptr_quals,
                TypeFlags::empty(),
                pointed_base,
                0, 0
            );
        }

        let mut saved_params = None;

        //
        // Suffix: array dims or function params - applied to base_ty
        //
        let ty = if self.current_token.kind == TK::LParen {
            //
            // Function declarator suffix
            //

            self.next(); // (
            let (params, is_variadic) = self.compile_params()?;
            self.expect(TK::RParen, "')'")?;

            let param_types = params.iter()
                .map(|(ty, ..)| *ty)
                .collect::<SmallVec<[_; MAX_PARAMS]>>();

            let param_start = self.types.alloc_params(&param_types);
            let param_count = params.len() as _;

            saved_params = Some(params);

            self.types.make_func(pointed_base, param_start, param_count, is_variadic)
        } else if self.current_token.kind == TK::LSquare {
            self.next(); // [

            if self.current_token.kind == TK::RSquare {
                // int arr[]

                self.next(); // ]
                self.types.array_of(pointed_base, 0)
            } else if self.current_token.kind == TK::Number
                || !self.is_hash_a_local_or_a_global(self.current_token.hash)
            {
                // int arr[1]

                let len = self.try_parse_and_eval_const_int()? as u32;
                self.expect(TK::RSquare, "']'")?;
                let ty = self.types.array_of(pointed_base, len);
                self.compile_array_dim_loop(ty)?
            } else {
                // int n = 10;
                // int arr[n];

                //
                // No VLAs in global context!
                //
                if self.in_global_context {
                    return Err(CError::VlaInGlobalContext { span: self.current_token.span });
                }

                self.compile_expr_no_comma()?;
                self.expect(TK::RSquare, "']'")?;

                let (r, _) = self.pop_reg()?;
                let dim_off = self.locals.alloc_vla(HASH_HIDDEN_LOCAL, TYPE_LONG);
                self.code.mov_store(Reg::Rbp, dim_off, r, true);
                self.regs.free(r);

                let ty = self.types.alloc_fresh(
                    TypeKind::Array, QualFlags::empty(), TypeFlags::empty(),
                    pointed_base, 0, dim_off as u32
                );
                self.compile_array_dim_loop(ty)?
            }
        } else {
            pointed_base
        };

        //
        // Patch grouped declarator: replace TYPE_VOID placeholder with ty
        //
        let ty = if let Some(inner_ty) = inner {
            self.patch_declarator_base(inner_ty, ty)
        } else {
            ty
        };

        Ok((ty, name_tok, saved_params))
    }

    /// Replace TYPE_VOID placeholder leaves (from grouped declarator parsing)
    /// with real_base throughout the type chain.
    #[inline]
    fn patch_declarator_base(&mut self, ty: TypeRef, real_base: TypeRef) -> TypeRef {
        if ty == TYPE_VOID { return real_base; }

        match self.get_kind(ty) {
            TypeKind::Ptr => {
                let e = *self.types.get(ty);
                let patched = self.patch_declarator_base(e.ref_, real_base);
                self.types.intern(TypeKind::Ptr, e.quals, TypeFlags::empty(), patched, 0, 0)
            }

            TypeKind::Array => {
                let e = *self.types.get(ty);
                let patched = self.patch_declarator_base(e.elem(), real_base);
                if e.is_vla() {
                    self.types.alloc_fresh(
                        TypeKind::Array, e.quals, e.flags,
                        patched, 0, e.extra2
                    )
                } else {
                    self.types.array_of(patched, e.array_len())
                }
            }

            TypeKind::Func => {
                let e = *self.types.get(ty);
                let patched_ret = self.patch_declarator_base(e.ret_ty(), real_base);
                self.types.make_func(
                    patched_ret, e.param_start(), e.param_count(),
                    e.flags.contains(TypeFlags::VARIADIC)
                )
            }

            _ => ty
        }
    }

    // Materialize a CValue into a register
    #[inline]
    fn force_gp(&mut self, v: CValue) -> CResult<Reg> {
        match v.kind() {
            VK::Reg => match v.reg() {
                ValReg::Gp(r) => Ok(r),
                ValReg::Xmm(_) => unreachable!("float in force_reg"),
            },
            VK::Imm => {
                let r = self.regs.alloc(Span::POISONED)?;
                self.code.mov_ri64(r, v.imm);
                Ok(r)
            }
            VK::Local | VK::RegInd => {
                let base = v.reg().as_gp();
                let r = self.regs.alloc(Span::POISONED)?;
                self.emit_int_load(r, base, v.offset(), v.ty);
                self.regs.free(base);  // Free the pointer register after dereferencing
                Ok(r)
            }
        }
    }

    // Materialize a CValue into an XMM register
    #[inline]
    fn force_xmm(&mut self, v: CValue) -> CResult<XmmReg> {
        match v.kind() {
            VK::Reg => match v.reg() {
                ValReg::Xmm(r) => Ok(r),
                ValReg::Gp(_) => unreachable!("int in force_xmm"),
            },

            VK::Imm => {
                // Float immediate - store in rodata and load

                let xmm = self.xmms.alloc(Span::POISONED)?;
                let rodata_off = self.rodata.len() as u32;
                match self.get_kind(v.ty) {
                    TypeKind::Float => self.rodata.extend_from_slice(&(v.get_fimm() as f32).to_bits().to_le_bytes()),
                    _               => self.rodata.extend_from_slice(&v.get_fimm().to_bits().to_le_bytes()),
                }
                let text_off = self.emit_float_load_rip(xmm, v.ty) as _;
                self.rodata_relocs.push(RodataReloc { text_off, rodata_off });

                Ok(xmm)
            }

            VK::Local | VK::RegInd => {
                let base = match v.reg() { ValReg::Gp(r) => r, _ => unreachable!() };
                let xmm = self.xmms.alloc(Span::POISONED)?;
                self.emit_float_load(xmm, base, v.offset(), v.ty);
                if v.kind() == VK::RegInd { self.regs.free(base); }
                Ok(xmm)
            }
        }
    }

    #[inline]
    fn coerce_to_xmm(&mut self, v: CValue, target_ty: TypeRef) -> CResult<XmmReg> {
        let v_kind = self.get_kind(v.ty);
        let target_kind = self.get_kind(target_ty);

        if self.is_float(v.ty) {
            let r = self.force_xmm(v)?;

            // @Incomplete

            // Still need to convert if types differ
            if v_kind == TypeKind::Double && target_kind == TypeKind::Float {
                self.code.cvtsd2ss(r, r);
            } else if v_kind == TypeKind::Float && target_kind == TypeKind::Double {
                self.code.cvtss2sd(r, r);
            }

            return Ok(r);
        }

        //
        // int -> float
        //
        let gp = self.force_gp(v)?;
        let xmm = self.xmms.alloc(Span::POISONED)?;
        self.code.cvtsi2sd(xmm, gp);  // Always convert to double first
        self.regs.free(gp);
        if target_kind == TypeKind::Float {
            self.code.cvtsd2ss(xmm, xmm);
        }

        Ok(xmm)
    }

    #[inline]
    fn free_reg(&mut self, reg: ValReg) {
        match reg {
            ValReg::Gp(r)  => self.regs.free(r),
            ValReg::Xmm(r) => self.xmms.free(r),
        }
    }

    #[inline]
    fn pop_reg(&mut self) -> CResult<(Reg, TypeRef)> {
        let v = self.vstack.pop();
        Ok((self.force_gp(v)?, v.ty))
    }

    #[inline]
    fn pop_xmm(&mut self) -> CResult<(XmmReg, TypeRef)> {
        let v = self.vstack.pop();
        Ok((self.force_xmm(v)?, v.ty))
    }

    #[inline]
    pub fn compile(&mut self) {
        while !self.at_eof() {
            if let Err(e) = self.compile_top_level() {
                e.emit(&self.pp.src_arena);
                std::process::exit(1);
            }
        }

        if !self.pp.ifdef_stack.is_empty() {
            eprintln!("warning: {} unclosed #if/#ifdef at end of file",
                      self.pp.ifdef_stack.len());
            for (i, s) in self.pp.ifdef_stack.iter().enumerate() {
                eprintln!("  [{}] {:?}", i, s);
            }
        }
    }

    fn compile_top_level(&mut self) -> CResult<()> {
        bitflags::bitflags! {
            #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
            pub struct TopLevelFlags: u8 {
                const EXTERN  = 0x2;
                const STATIC  = 0x8;
                const INLINE  = 0x10; // no-op
            }
        }

        self.in_global_context = true;

        if self.current_token.hash == HASH_TYPEDEF {
            return self.compile_typedef();
        }

        if self.current_token.hash == HASH_STRUCT {
            _ = self.compile_struct()?;
            self.expect(TK::SemiColon, "';'")?;
            return Ok(());
        }

        if self.current_token.hash == HASH_ENUM {
            _ = self.compile_enum()?;
            self.expect(TK::SemiColon, "';'")?;
            return Ok(());
        }

        if self.current_token.hash == HASH_UNION {
            _ = self.compile_union()?;
            self.expect(TK::SemiColon, "';'")?;
            return Ok(());
        }

        //
        // Consume specifiers
        //
        let mut top_flags = TopLevelFlags::empty();
        loop {
            match self.current_token.hash {
                HASH_EXTERN => { top_flags.insert(TopLevelFlags::EXTERN); self.next(); }
                HASH_STATIC => { top_flags.insert(TopLevelFlags::STATIC); self.next(); }
                HASH_INLINE => { top_flags.insert(TopLevelFlags::INLINE); self.next(); }
                _ => break,
            }
        }

        let base_ty = self.compile_base_type()?;
        let (decl_ty, name_tok_opt, params_opt) = self.parse_declarator_impl(base_ty)?;
        let name_tok = name_tok_opt.ok_or_else(|| CError::Expected {
            span: self.current_token.span,
            expected: "function or variable name",
            got: self.s(self.current_token).to_owned(),
        })?;

        let mut flags = SymFlags::empty();
        if top_flags.contains(TopLevelFlags::STATIC) { flags.insert(SymFlags::STATIC); }
        if top_flags.contains(TopLevelFlags::EXTERN) { flags.insert(SymFlags::EXTERN); }

        if let Some(params) = params_opt {
            //
            // Function declaration
            //

            let func_ty = decl_ty;

            if self.current_token.kind == TK::SemiColon {
                self.expect(TK::SemiColon, "';'")?;
                let name: SmallString<[_; 24]> = name_tok.s(&self.src_arena).into();
                flags.insert(SymFlags::EXTERN);  // Forward declaration should always be extern.
                self.syms.insert(&name, 0, 0, flags, Some(func_ty));
                return Ok(());
            }

            flags -= SymFlags::EXTERN;
            return self.compile_func(name_tok.span, name_tok.hash, func_ty, params, flags);
        }

        //
        // Global variable
        //

        self.compile_global_decl(
            decl_ty, name_tok,
            top_flags.contains(TopLevelFlags::EXTERN),
            top_flags.contains(TopLevelFlags::STATIC)
        )
    }

    fn compile_func(
        &mut self,
        name_span: Span,
        hash: u64,
        func_ty: TypeRef,
        params: Vec<(TypeRef, u64, Span)>,
        flags: SymFlags,
    ) -> CResult<()> {
        let code_off = self.code.pos() as u32;
        let mut code_len = 0;

        let name: SmallString<[_; 24]> = name_span.s(&self.src_arena).into(); // @Borrow

        //
        // Store code length as 0 for now (patch up later)
        //

        let (sym_index, func_ty) = if let Some(forward_declaration_symbol_index) = self.syms.find(hash) {
            // @Incomplete
            // TODO(#22): Verify that func's forward decl and definition signatures match

            self.syms.patch_forward_declaration(
                forward_declaration_symbol_index,
                code_off, code_len, flags - SymFlags::EXTERN
            );

            let forward_func_ty = self.syms[forward_declaration_symbol_index].func_ty;
            self.types.patch_func_forward_decl(
                forward_func_ty,
                if self.get(func_ty).is_variadic() { TypeFlags::VARIADIC } else { TypeFlags::empty() }
            );

            (forward_declaration_symbol_index, forward_func_ty)
        } else {
            let sym_index = self.syms.insert(
                &name,
                code_off,
                code_len,
                flags,
                Some(func_ty)
            );

            (sym_index, func_ty)
        };

        let ret_ty = self.types.get(func_ty).ret_ty();

        self.locals = LocalTable::new();
        self.regs   = RegAlloc::new();
        self.ret_ty = ret_ty;
        self.vla_sizes = Default::default();
        self.in_global_context = false;

        //
        // Prologue
        //
        self.code.push_r(Reg::Rbp);
        self.code.mov_rr(Reg::Rbp, Reg::Rsp);
        let frame_patch = self.code.pos() + 3; // imm32 inside sub rsp, imm32
        self.code.sub_rsp(0);

        let mut argc     = 0usize;
        let mut xmm_argc = 0usize;
        let mut stack_param_off = 16i32; // rbp+0 = saved rbp, rbp+8 = return addr, rbp+16 = first stack param

        //
        //
        // Handle struct return - hidden pointer arrives in rdi
        //
        //

        let is_memory_ret = matches!(
            self.get_kind(ret_ty), TypeKind::Struct | TypeKind::Union
        ) && self.classify_struct(ret_ty)[0] == SysVClass::Memory;

        if is_memory_ret {
            //
            // Save the sret pointer
            //

            let ret_ptr_off = self.locals.alloc(
                HASH_HIDDEN_LOCAL,
                self.types.ptr_to(ret_ty),
                &self.types
            );
            self.code.mov_store(Reg::Rbp, ret_ptr_off, ARG_REGS[0], true);
            self.ret_ptr_off = Some(ret_ptr_off);

            argc = 1;
        }

        //
        // Spill params to stack (convenience, for pop_reg)
        //

        for &(param_ty, param_hash, param_span) in &params {
            if matches!(self.get_kind(param_ty), TypeKind::Struct | TypeKind::Union) {
                let classes = self.classify_struct(param_ty);

                if classes[0] == SysVClass::Memory {
                    //
                    // Param arrived as pointer in integer reg - copy into local
                    //

                    let size = self.types.size_of(param_ty) as i32;
                    let aligned = (size + 15) & !15;

                    let off = self.locals.alloc(param_hash, param_ty, &self.types);
                    self.emit_struct_copy(Reg::Rbp, off, Reg::Rbp, stack_param_off, size)?;

                    stack_param_off += aligned;
                } else {
                    //
                    // Param arrived in registers - unpack into local
                    //

                    let off = self.locals.alloc(param_hash, param_ty, &self.types);
                    self.unpack_struct_from_arg_regs(
                        off, param_ty,
                        &mut argc, &mut xmm_argc,
                        name_span, param_span
                    )?;
                }
            } else if self.is_float(param_ty) {
                let off = self.locals.alloc(param_hash, param_ty, &self.types);
                self.emit_float_store(Reg::Rbp, off, XMM_ARG_REGS[xmm_argc], param_ty);
                xmm_argc += 1;
            } else {
                let off = self.locals.alloc(param_hash, param_ty, &self.types);
                self.emit_int_store(Reg::Rbp, off, ARG_REGS[argc], param_ty);
                argc += 1;
            }
        }

        self.expect(TK::LCurly, "'{'")?;
        while self.current_token.kind != TK::RCurly && !self.at_eof() {
            if let Err(e) = self.compile_stmt() {
                e.emit(&self.pp.src_arena);
                std::process::exit(1);
            }
        }
        self.expect(TK::RCurly, "'}'")?;

        //
        // Fall-through epilogue
        //
        self.code.xor_rr(Reg::Rax, Reg::Rax);  // Implicit return 0
        self.code.mov_rr(Reg::Rsp, Reg::Rbp);
        self.code.pop_r(Reg::Rbp);
        self.code.ret();

        let frame = align(self.locals.frame_bytes as _, 16) as _;
        self.code.patch_i32(frame_patch, frame);

        //
        // Patch up the code length
        //
        code_len = self.code.bytes.len() as u32 - code_off;
        self.syms[sym_index].code_len = code_len;

        Ok(())
    }

    #[inline]
    fn compile_params(&mut self) -> CResult<(Vec<(TypeRef, u64, Span)>, bool)> {
        let mut params = Vec::new();

        if self.current_token.kind == TK::RParen {
            return Ok((params, false));
        }
        if self.current_token.hash == HASH_VOID && self.next_token.kind    == TK::RParen {
            self.next();
            return Ok((params, false));
        }

        let mut variadic = false;
        loop {
            let base = self.compile_base_type()?;
            let (ty, name_tok) = self.parse_declarator(base)?;

            // Array params decay to pointer
            let ty = if self.types.get_kind(ty) == TypeKind::Array {
                self.types.ptr_to(self.types.get(ty).elem())
            } else {
                ty
            };

            let (hash, span) = match name_tok {
                Some(t) => (t.hash, t.span),
                None    => (0, Span::POISONED),
            };
            params.push((ty, hash, span));

            if self.current_token.kind != TK::Comma { break; }
            self.next();
            if self.current_token.kind == TK::TripleDot { variadic = true; self.next(); break; }
        }

        Ok((params, variadic))
    }

    #[inline]
    fn can_hash_start_a_type(&self, h: u64) -> bool {
        HASHES_THAT_START_TYPES.contains(&h) || self.typedefs.contains_key(&h) || self.tags.contains_key(&h)
    }

    #[inline]
    fn compile_stmt(&mut self) -> CResult<()> {
        match self.current_token.kind {
            TK::Ident => {
                let h = self.current_token.hash;
                     if h == HASH_RETURN        { self.compile_return()     }
                else if h == HASH_IF            { self.compile_if()         }
                else if h == HASH_FOR           { self.compile_for()        }
                else if h == HASH_WHILE         { self.compile_while()      }
                else if h == HASH_BREAK         { self.compile_break(self.current_token.span)    }
                else if h == HASH_CONTINUE      { self.compile_continue(self.current_token.span) }
                else if self.can_hash_start_a_type(h) { self.compile_local_decl() }
                else { self.compile_expr_stmt()  }
            }

            TK::LCurly => self.compile_block(),

            TK::SemiColon => { self.next(); Ok(()) }

            _          => self.compile_expr_stmt(),
        }
    }

    #[inline]
    fn compile_block(&mut self) -> CResult<()> {
        self.expect(TK::LCurly, "'{'")?;
        self.locals.push_scope();

        while self.current_token.kind != TK::RCurly && !self.at_eof() {
            if let Err(e) = self.compile_stmt() {
                e.emit(&self.pp.src_arena);
                std::process::exit(1);
            }
        }

        self.locals.pop_scope();
        self.expect(TK::RCurly, "'}'").map(|_| ())
    }

    #[inline]
    fn compile_return(&mut self) -> CResult<()> {
        self.next(); // return

        let mut has_parens = false;
        if self.current_token.kind == TK::LParen { self.next(); has_parens = true }

        let ret_ty = self.ret_ty;
        let is_struct = matches!(self.get_kind(ret_ty), TypeKind::Struct | TypeKind::Union);

        if self.current_token.kind != TK::SemiColon {
            self.compile_expr()?;

            if is_struct {
                let v = self.vstack.pop();
                let size = self.types.size_of(ret_ty) as i32;

                let (src_base, src_off) = match v.kind() {
                    VK::Local | VK::RegInd => (v.reg().as_gp(), v.offset()),
                    VK::Reg  => (v.reg().as_gp(), 0),
                    VK::Imm  => unreachable!(),
                };

                let classes = self.classify_struct(ret_ty);
                if classes[0] == SysVClass::Memory {
                    //
                    // The struct is big, just copy it into dst.
                    //

                    let ret_ptr_off = self.ret_ptr_off.unwrap();
                    let dst = self.regs.alloc(Span::POISONED)?;
                    self.code.mov_load(dst, Reg::Rbp, ret_ptr_off, true);
                    self.emit_struct_copy(dst, 0, src_base, src_off, size)?;
                    self.code.mov_rr(Reg::Rax, dst);
                    self.regs.free(dst);
                } else {
                    //
                    // The struct isn't that big, pack it into registers.
                    //

                    self.pack_struct_into_return_regs(src_base, src_off, ret_ty)?;
                }

                if v.kind() == VK::RegInd { self.regs.free(v.reg().as_gp()); }
            } else if self.is_float(ret_ty) {
                let v = self.vstack.pop();
                let r = self.coerce_to_xmm(v, ret_ty)?;
                self.emit_float_mov(XmmReg::Xmm0, r, ret_ty);
                self.xmms.free(r);
            } else {
                let v = self.pop_vstack_and_decay_array()?;
                let v = self.coerce_for_assign(v, ret_ty)?;
                let r = self.force_gp(v)?;
                self.code.mov_rr(Reg::Rax, r);
                self.regs.free(r);
            }
        }

        if has_parens { self.expect(TK::RParen, "')'")?; }

        self.expect(TK::SemiColon, "';'")?;
        self.code.mov_rr(Reg::Rsp, Reg::Rbp);
        self.code.pop_r(Reg::Rbp);
        self.code.ret();

        Ok(())
    }

    #[inline]
    fn compile_if(&mut self) -> CResult<()> {
        self.next(); // if

        self.expect(TK::LParen, "'('")?; // (
        self.compile_expr_no_comma()?;
        self.expect(TK::RParen, "')'")?; // )

        let (r, _) = self.pop_reg()?;
        self.code.test_rr(r);
        self.free_reg(ValReg::Gp(r));

        let je_patch = self.code.je_rel32(); // je .else_or_end

        self.compile_stmt()?; // then

        if self.current_token.hash == HASH_ELSE {
            self.next(); // else
            let jmp_patch = self.code.jmp_rel32(); // jmp .end
            self.code.patch_rel32(je_patch, self.code.pos());
            self.compile_stmt()?;  // else-body
            self.code.patch_rel32(jmp_patch, self.code.pos());
        } else {
            self.code.patch_rel32(je_patch, self.code.pos());
        }

        Ok(())
    }

    #[inline]
    fn compile_break(&mut self, span: Span) -> CResult<()> {
        self.next(); // break
        self.expect(TK::SemiColon, "';'")?;

        let ctx = self.loop_stack.last_mut().ok_or(CError::BreakOutsideLoop { span })?;
        let patch = self.code.jmp_rel32();
        ctx.break_patches.push(patch as _);

        Ok(())
    }

    #[inline]
    fn compile_continue(&mut self, span: Span) -> CResult<()> {
        self.next(); // continue
        self.expect(TK::SemiColon, "';'")?;

        let ctx = self.loop_stack.last_mut().ok_or(CError::ContinueOutsideLoop { span })?;
        let patch = self.code.jmp_rel32();
        ctx.continue_patches.push(patch as _);

        Ok(())
    }

    fn compile_while(&mut self) -> CResult<()> {
        self.next(); // while

        //
        // Collect cond tokens
        //
        let (_, cond_toks) = self.collect_tokens_inside_parens()?;

        //
        // Jmp to cond
        //
        let jmp_cond = self.code.jmp_rel32();
        let loop_top = self.code.pos();

        //
        // Body
        //

        self.loop_stack.push(Default::default());

        self.compile_stmt()?;

        let ctx = self.loop_stack.pop().unwrap();

        let cond_top = self.code.pos();
        for patch in ctx.continue_patches {
            self.code.patch_rel32(patch as _, cond_top);
        }

        // @Cutnpaste from compile_for
        //
        // Cond
        //
        self.code.patch_rel32(jmp_cond, self.code.pos());
        if !cond_toks.is_empty() {
            let saved_cur  = self.pp.current_token;
            let saved_peek = self.pp.next_token;

            let mut replay = cond_toks;
            replay.push(saved_cur);
            replay.push(saved_peek);

            self.pp.exp.push(replay.as_slice());
            self.pp.current_token = self.pp.cook();
            self.pp.next_token    = self.pp.cook();

            self.compile_expr_no_comma()?;
            let (r, _) = self.pop_reg()?;
            self.code.test_rr(r);
            self.regs.free(r);
        }

        let jne_patch = self.code.jne_rel32();
        self.code.patch_rel32(jne_patch, loop_top);

        // Patch all break jumps to here
        for patch in ctx.break_patches {
            self.code.patch_rel32(patch as _, self.code.pos());
        }

        Ok(())
    }

    fn compile_for(&mut self) -> CResult<()> {
        self.next(); // for
        self.expect(TK::LParen, "'('")?;

        //
        // Init
        //

        // Init has its own scope
        self.locals.push_scope();

        if self.current_token.kind != TK::SemiColon {
            let h = self.current_token.hash;
            if self.can_hash_start_a_type(h) {
                self.compile_local_decl()?;
            } else {
                self.compile_expr()?;
                let v = self.vstack.pop();
                if v.kind() == VK::Reg { self.free_reg(v.reg()); }
                self.expect(TK::SemiColon, "';'")?;
            }
        } else {
            self.next(); // ';'
        }

        //
        // Collect cond tokens
        //
        let mut cond_toks = Vec::new();
        while self.current_token.kind != TK::SemiColon && !self.at_eof() {
            cond_toks.push(self.next());
        }
        self.expect(TK::SemiColon, "';'")?;

        //
        // Collect post tokens
        //
        let mut post_toks = Vec::new();
        let mut depth = 0usize;
        loop {
            match self.current_token.kind {
                TK::LParen              => { depth += 1; post_toks.push(self.next()); }
                TK::RParen if depth > 0 => { depth -= 1; post_toks.push(self.next()); }
                TK::RParen              => { self.next(); break; }
                TK::Eof                 => break,
                _                       => { post_toks.push(self.next()); }
            }
        }

        //
        // Jmp to cond
        //
        let jmp_cond = self.code.jmp_rel32();
        let loop_top = self.code.pos();

        //
        // Body
        //

        self.loop_stack.push(Default::default());

        self.compile_stmt()?;

        //
        // Post
        //

        let post_top = self.code.pos();

        let ctx = self.loop_stack.pop().unwrap();

        // Patch continue jumps to post
        for patch in ctx.continue_patches {
            self.code.patch_rel32(patch as _, post_top);
        }

        if !post_toks.is_empty() {
            let saved_cur  = self.pp.current_token;
            let saved_peek = self.pp.next_token;

            let mut replay = post_toks;
            replay.push(saved_cur);
            replay.push(saved_peek);

            self.pp.exp.push(replay.as_slice());
            self.pp.current_token = self.pp.cook();
            self.pp.next_token    = self.pp.cook();

            self.compile_expr()?;
            let v = self.vstack.pop();
            if matches!(v.kind(), VK::Reg | VK::RegInd) {
                self.free_reg(v.reg());
            }
        }

        //
        // Cond
        //
        self.code.patch_rel32(jmp_cond, self.code.pos());
        if cond_toks.is_empty() {
            //
            // Cond tokens are empty: for (.. ;; ..)
            // this is an infinite loop.
            //

            let jmp = self.code.jmp_rel32();
            self.code.patch_rel32(jmp, loop_top);
        } else {
            let saved_cur  = self.pp.current_token;
            let saved_peek = self.pp.next_token;

            let mut replay = cond_toks;
            replay.push(saved_cur);
            replay.push(saved_peek);

            self.pp.exp.push(replay.as_slice());
            self.pp.current_token = self.pp.cook();
            self.pp.next_token    = self.pp.cook();

            self.compile_expr_no_comma()?;
            let (r, _) = self.pop_reg()?;
            self.code.test_rr(r);
            self.regs.free(r);
        }

        let jne_patch = self.code.jne_rel32();
        self.code.patch_rel32(jne_patch, loop_top);

        // Patch continue jumps to end
        for patch in ctx.break_patches {
            self.code.patch_rel32(patch as _, self.code.pos());
        }

        // Exit init's scope
        self.locals.pop_scope();

        Ok(())
    }

    #[inline]
    fn emit_extend(&mut self, r: Reg, from_size: u32, to_size: u32, is_signed: bool) {
        match (from_size, to_size, is_signed) {
            (1, _, true)  => self.code.movsx_r8(r, r),
            (2, _, true)  => self.code.movsx_r16(r, r),
            (4, 8, true)  => self.code.movsxd(r, r),
            (1, _, false) => self.code.movzx_r8(r, r),
            (2, _, false) => self.code.movzx_r16(r, r),
            (4, 8, false) => {} // mov r32, r32 is a no-op since mov_rr skips dst==src,
            // but zero-extension is implicit on x86-64 for any 32-bit write
            // so just leave the value as-is
            _ => {}
        }
    }

    #[inline]
    fn emit_store_impl(&mut self, base: Reg, off: i32, ty: TypeRef, keep: bool) -> CResult<()> {
        if matches!(self.get_kind(ty), TypeKind::Struct | TypeKind::Union) {
            let v = self.vstack.pop();
            let size = self.types.size_of(ty) as i32;

            let (src_base, src_off) = match v.kind() {
                VK::Local | VK::RegInd => (v.reg().as_gp(), v.offset()),
                VK::Reg                => (v.reg().as_gp(), 0),

                VK::Imm => unreachable!(),
            };

            //
            // Don't free yet: emit_struct_copy is using it!
            //
            // if v.kind() == VK::RegInd { ... }
            self.emit_struct_copy(base, off, src_base, src_off, size)?;
            if v.kind() == VK::RegInd { self.regs.free(v.reg().as_gp()); }

            if keep {
                let dst_r = self.regs.alloc(Span::POISONED)?;
                self.code.lea(dst_r, base, off);
                self.vstack.push(CValue::regind(ty, dst_r, 0));
            }

            return Ok(());
        }

        if !self.is_float(ty) {
            let v = self.pop_vstack_and_decay_array()?;
            let v = self.coerce_for_assign(v, ty)?;
            let r = self.force_gp(v)?;

            self.emit_int_store(base, off, r, ty);

            if keep {
                self.vstack.push(CValue::gp(ty, r));
            } else {
                self.regs.free(r);
            }

            return Ok(());
        }

        let v = self.vstack.pop();
        let v = self.coerce_for_assign(v, ty)?;
        let r = self.coerce_to_xmm(v, ty)?;

        self.emit_float_store(base, off, r, ty);

        if keep {
            self.vstack.push(CValue::xmm(ty, r));
        } else {
            self.xmms.free(r);
        }

        Ok(())
    }

    #[inline]
    fn emit_store(&mut self, base: Reg, off: i32, ty: TypeRef) -> CResult<()> {
        self.emit_store_impl(base, off, ty, false)
    }

    #[inline]
    fn emit_store_keep(&mut self, base: Reg, off: i32, ty: TypeRef) -> CResult<()> {
        self.emit_store_impl(base, off, ty, true)
    }

    #[inline]
    fn compile_typedef(&mut self) -> CResult<()> {
        self.next(); // typedef

        let base_ty = self.compile_base_type()?;

        if self.current_token.kind == TK::SemiColon {
            emit_diag_warning("typedef requires a name", self.current_token.span, &self.src_arena);
            self.next();
            return Ok(());
        }

        let (ty, name_tok) = self.parse_declarator(base_ty)?;
        let name_tok = name_tok.ok_or_else(|| CError::Expected {
            span: self.current_token.span,
            expected: "typedef name",
            got: self.s(self.current_token).to_owned(),
        })?;

        self.expect(TK::SemiColon, "';'")?;

        self.typedefs.insert(name_tok.hash, ty);

        if matches!(
            self.get_kind(ty),
            TypeKind::Struct | TypeKind::Union | TypeKind::Enum
        ) {
            //
            // @Note: See typenames note in TypeTable's struct definition.
            //

            let name: SmallString<_> = name_tok.s(&self.src_arena).into();
            self.types.set_type_name(ty, name);
        }

        Ok(())
    }

    fn compile_global_decl(
        &mut self,
        first_ty: TypeRef,
        first_name_tok: Token,
        is_extern: bool,
        is_static: bool,
    ) -> CResult<()> {
        let mut ty       = first_ty;
        let mut name_tok = first_name_tok;

        loop {
            let name: SmallString<[_; 24]> = name_tok.s(&self.src_arena).into();
            let hash = name_tok.hash;

            self.compile_global_decl_impl(ty, &name, hash, is_extern, is_static)?;

            if self.current_token.kind != TK::Comma { break; }
            self.next(); // ,

            //
            // Parse the next declarator in the comma list
            // base type is already known - parse_declarator gives us name + type modifier
            //

            let (next_ty, next_name_opt) = self.parse_declarator(first_ty)?;
            name_tok = next_name_opt.ok_or_else(|| CError::Expected {
                span: self.current_token.span,
                expected: "variable name",
                got: self.s(self.current_token).to_owned(),
            })?;
            ty = next_ty;
        }

        self.expect(TK::SemiColon, "';'").map(|_| ())
    }

    fn compile_global_decl_impl(
        &mut self,
        ty_ref: TypeRef,
        name: &str,
        hash: u64,
        is_extern: bool,
        is_static: bool,
    ) -> CResult<()> {
        if is_extern {
            //
            // @Incomplete
            // Extern global - treat like extern function, add to sym table
            // for now skip - extern globals need GOT relocs which is more complex
            //
            while !matches!(
                self.current_token.kind,
                TK::SemiColon | TK::Comma | TK::Eof) {
                self.next();
            }

            return Ok(());
        }

        let ty = self.get_kind(ty_ref);
        let size = self.size_of(ty_ref) as usize;

        let has_initializer = self.current_token.kind == TK::Eq;
        if !has_initializer {
            //
            // No initializer - goes into .bss
            //

            if ty == TypeKind::Array {
                let arr_len = self.types.get(ty_ref).array_len() as usize;
                let inferred = arr_len == 0;  // @Note: Shouldn't be a VLA since its a global..?

                if inferred {
                    todo!() // @Error out!
                }
            }

            let off = self.bss_size;

            self.bss_size += size;
            self.globals.insert(name, hash, ty_ref, off as u32, true, is_static);

            return Ok(())
        }

        self.next(); // =

        // Strip compound literal prefix: (Type){...} -> {..}
        if self.current_token.kind == TK::LParen
            && self.can_hash_start_a_type(self.next_token.hash)
        {
            self.next(); // (
            let _ = self.compile_type()?;
            if self.current_token.kind == TK::RParen {
                self.next(); // )
            }
        }

        //
        // Constant initializer only (@Incomplete: Check for that)
        //

        let (is_bss, data_off) = match ty {
            TypeKind::Int | TypeKind::Short | TypeKind::Long |
            TypeKind::LLong | TypeKind::Char => {
                let v = self.try_parse_and_eval_const_int()?;
                if v == 0 {
                    let off = self.bss_size;
                    self.bss_size += size;

                    (true, off as u32)
                } else {
                    let off = self.data.len() as u32;
                    match ty {
                        TypeKind::Char  => self.data.push(v as u8),
                        TypeKind::Short => self.data.extend_from_slice(&(v as i16).to_le_bytes()),
                        TypeKind::Int   => self.data.extend_from_slice(&(v as i32).to_le_bytes()),
                        _               => self.data.extend_from_slice(&v.to_le_bytes()),
                    }

                    (false, off)
                }
            }

            TypeKind::Float | TypeKind::Double => {
                let v = self.try_parse_eval_const_float()?;
                if v == 0.0 {
                    let off = self.bss_size;
                    self.bss_size += self.types.size_of(ty_ref) as usize;
                    (true, off as u32)
                } else {
                    let off = self.data.len() as u32;
                    match ty {
                        TypeKind::Float => self.data.extend_from_slice(&(v as f32).to_bits().to_le_bytes()),
                        _               => self.data.extend_from_slice(&v.to_bits().to_le_bytes()),
                    }
                    (false, off)
                }
            }

            TypeKind::Array => {
                // Has initializer
                let off = self.data.len() as u32;

                let elem_ty = self.types.get(ty_ref).elem();
                let elem_sz = self.types.size_of(elem_ty) as usize;
                let arr_len = self.types.get(ty_ref).array_len() as usize;
                let inferred = arr_len == 0;  // @Note: Shouldn't be a VLA since its a global..?

                let actual_len = if self.current_token.kind == TK::StrLit &&
                    self.types.get_kind(elem_ty) == TypeKind::Char
                {
                    //
                    // Initialized global array of bytes (string)
                    //

                    let t   = self.next();
                    let raw = t.s(&self.pp.src_arena);
                    let s   = &raw[1..raw.len()-1];

                    // @Cutnpaste
                    // Unescape into data
                    let bytes = s.as_bytes();
                    let mut i = 0;
                    while i < bytes.len() {
                        let b = if bytes[i] == b'\\' && i + 1 < bytes.len() {
                            i += 1;
                            match bytes[i] {
                                b'n' => b'\n', b't' => b'\t', b'r' => b'\r',
                                b'0' => b'\0', b'\\' => b'\\',
                                b'\'' => b'\'', b'"' => b'"',
                                other => other,
                            }
                        } else { bytes[i] };
                        self.data.push(b);
                        i += 1;
                    }

                    self.data.push(0);

                    let actual_len = Self::unescape_len(s) + 1;

                    // Zero pad remaining if sized
                    if !inferred && actual_len < arr_len {
                        self.data.extend(std::iter::repeat(0u8).take(arr_len - actual_len));
                    }

                    actual_len
                } else {
                    //
                    // Initialized global array
                    //

                    self.next(); // {

                    let mut count = 0usize;
                    while self.current_token.kind != TK::RCurly && !self.at_eof() {
                        if !inferred && count >= arr_len {
                            // Too many, error out..?
                            self.compile_expr()?;
                            self.vstack.pop();
                        } else {
                            let v = self.try_parse_and_eval_const_int()?;
                            match self.types.get_kind(elem_ty) {  // @Cutnpaste from above
                                TypeKind::Char  => self.data.push(v as u8),
                                TypeKind::Short => self.data.extend_from_slice(&(v as i16).to_le_bytes()),
                                TypeKind::Int   => self.data.extend_from_slice(&(v as i32).to_le_bytes()),
                                _               => self.data.extend_from_slice(&v.to_le_bytes()),
                            }
                            count += 1;
                        }

                        if self.current_token.kind == TK::Comma { self.next(); }
                    }

                    self.expect(TK::RCurly, "'}'")?;

                    let actual_len = if inferred { count } else { arr_len };

                    // Zero pad remaining if sized
                    if !inferred && count < arr_len {
                        let remaining = (arr_len - count) as usize * elem_sz;
                        self.data.extend(std::iter::repeat(0u8).take(remaining));
                    }

                    actual_len
                };

                let array_ty = self.types.array_of(elem_ty, actual_len as _);

                //
                // Insert the global here, with our computed type in case the size was inferred: int arr[] = {..}
                //
                self.globals.insert(name, hash, array_ty, off, false, is_static);
                return Ok(());
            }

            TypeKind::Struct | TypeKind::Union => {
                let off = self.data.len() as u32;
                let size = self.types.size_of(ty_ref) as usize;

                // Reserve space
                let start = self.data.len();
                self.data.resize(start + size, 0u8);

                self.expect(TK::LCurly, "'{'")?;

                let e = self.types.get(ty_ref);
                let (fstart, count) = (e.field_start(), e.field_count());
                let mut i = 0usize;

                while self.current_token.kind != TK::RCurly && !self.at_eof() {
                    if i < count as usize {
                        let field = *self.types.get_field_from_start(fstart, i as _);
                        let field_off = field.offset as usize;
                        let field_sz  = self.types.size_of(field.ty) as usize;

                        match self.get_kind(field.ty) {
                            TypeKind::Float | TypeKind::Double => {
                                let v = self.try_parse_eval_const_float()?;
                                let bytes = match self.get_kind(field.ty) {
                                    TypeKind::Float => (v as f32).to_bits().to_le_bytes().to_vec(),
                                    _               => v.to_bits().to_le_bytes().to_vec(),
                                };
                                self.data[start + field_off..start + field_off + field_sz]
                                    .copy_from_slice(&bytes);
                            }
                            _ => {
                                let v = self.try_parse_and_eval_const_int()?;
                                let bytes = v.to_le_bytes();
                                self.data[start + field_off..start + field_off + field_sz]
                                    .copy_from_slice(&bytes[..field_sz]);
                            }
                        }
                        i += 1;
                    } else {
                        // Too many - skip
                        self.try_parse_and_eval_const_int()?;
                    }

                    if self.current_token.kind == TK::Comma { self.next(); }
                }

                self.expect(TK::RCurly, "'}'")?;
                self.globals.insert(name, hash, ty_ref, off, false, is_static);
                return Ok(());
            }

            _ => {  // @Incomplete: Just skip this one
                while !matches!( // @Cutnpaste from above
                    self.current_token.kind,
                    TK::SemiColon | TK::Comma | TK::Eof) {
                    self.next();
                }
                return Ok(());
            }
        };

        self.globals.insert(name, hash, ty_ref, data_off, is_bss, is_static);

        Ok(())
    }

    #[inline]
    fn collect_tokens_inside_curlies(&mut self) -> CResult<(u32, Vec<Token>)> {
        let mut toks = Vec::new();
        toks.push(self.expect(TK::LCurly, "'{'")?);

        let mut depth  = 1usize;
        let mut count  = 0u32;
        let mut expect_val = true;

        loop {
            let t = self.current_token;
            match t.kind {
                TK::LCurly => { depth += 1; toks.push(self.next()); }
                TK::RCurly if depth > 1 => { depth -= 1; toks.push(self.next()); }
                TK::RCurly => { toks.push(self.next()); break; }
                TK::Comma if depth == 1 => { expect_val = true; toks.push(self.next()); }
                TK::Eof => break,
                _ => {
                    if expect_val && depth == 1 { count += 1; expect_val = false; }
                    toks.push(self.next());
                }
            }
        }

        Ok((count, toks))
    }

    #[inline]
    fn collect_tokens_inside_parens(&mut self) -> CResult<(u32, Vec<Token>)> {
        let mut toks = Vec::new();
        toks.push(self.expect(TK::LParen, "'('")?);

        let mut depth  = 1usize;
        let mut count  = 0u32;
        let mut expect_val = true;

        loop {
            let t = self.current_token;
            match t.kind {
                TK::LParen => { depth += 1; toks.push(self.next()); }
                TK::RParen if depth > 1 => { depth -= 1; toks.push(self.next()); }
                TK::RParen => { toks.push(self.next()); break; }
                TK::Comma if depth == 1 => { expect_val = true; toks.push(self.next()); }
                TK::Eof => break,
                _ => {
                    if expect_val && depth == 1 { count += 1; expect_val = false; }
                    toks.push(self.next());
                }
            }
        }

        Ok((count, toks))
    }

    fn compile_initializer(&mut self, base_off: i32, ty: TypeRef) -> CResult<()> {
        match self.get_kind(ty) {
            TypeKind::Struct | TypeKind::Union => {
                if self.current_token.kind == TK::LCurly {
                    self.next(); // {

                    let e = self.types.get(ty);
                    let (start, count) = (e.field_start(), e.field_count());

                    let mut i = 0usize;
                    while self.current_token.kind != TK::RCurly && !self.at_eof() {
                        if i < count as usize {
                            let field = *self.types.get_field_from_start(start, i as _);
                            let field_off = base_off + field.offset as i32;
                            self.compile_initializer(field_off, field.ty)?;
                            i += 1;
                        } else {
                            self.compile_expr_no_comma()?;
                            self.vstack.pop();
                        }

                        if self.current_token.kind == TK::Comma { self.next(); }
                    }

                    //
                    // Zero remaining fields
                    //
                    // @CodeOptimization: Use a memset here if the struct is big
                    if i < count as usize {
                        let tmp = self.regs.alloc(Span::POISONED)?;
                        self.code.xor_rr(tmp, tmp);
                        for j in i..count as usize {
                            let field = *self.types.get_field_from_start(start, j as _);
                            let foff = base_off + field.offset as i32;
                            self.emit_int_store(Reg::Rbp, foff, tmp, field.ty);
                        }
                        self.regs.free(tmp);
                    }

                    self.expect(TK::RCurly, "'}'")?;
                } else {
                    // No braces - assign from expression
                    self.compile_expr_no_comma()?;
                    self.emit_store(Reg::Rbp, base_off, ty)?;
                }
            }

            TypeKind::Array => {
                self.compile_array_initializer(base_off, ty)?;
            }

            _ => {
                // Scalar - strip optional braces
                if self.current_token.kind == TK::LCurly {
                    self.next(); // {
                    self.compile_expr_no_comma()?;
                    if self.current_token.kind == TK::Comma { self.next(); }
                    self.expect(TK::RCurly, "'}'")?;
                } else {
                    self.compile_expr_no_comma()?;
                }
                self.emit_store(Reg::Rbp, base_off, ty)?;
            }
        }
        Ok(())
    }

    fn compile_array_initializer(&mut self, base_off: i32, arr_ty: TypeRef) -> CResult<u32> {
        let elem_ty  = self.types.get(arr_ty).elem();
        let elem_sz  = self.types.size_of(elem_ty) as i32;
        let arr_len  = self.types.get(arr_ty).array_len() as i32;
        let inferred = arr_len == 0;

        if self.types.get_kind(elem_ty) == TypeKind::Char
            && self.current_token.kind == TK::StrLit
        {
            //
            // Initialized global array of bytes (string)
            //

            let t   = self.next();
            let raw = t.s(&self.pp.src_arena);
            let s   = &raw[1..raw.len()-1];
            let bytes = s.as_bytes();

            let mut off = base_off;
            let mut i   = 0usize;

            while i < bytes.len() && (off - base_off) < arr_len * elem_sz {
                // @Refactor
                let b = if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    i += 1;
                    match bytes[i] {
                        b'n'  => b'\n',
                        b't'  => b'\t',
                        b'r'  => b'\r',
                        b'0'  => b'\0',
                        b'\\' => b'\\',
                        b'\'' => b'\'',
                        b'"'  => b'"',
                        other => other,
                    }
                } else {
                    bytes[i]
                };

                // store byte: mov byte [rbp + off], imm
                let tmp = self.regs.alloc(Span::POISONED)?;
                self.code.mov_ri64(tmp, b as i64);
                self.code.mov_store8(Reg::Rbp, off, tmp);
                self.regs.free(tmp);

                off += 1;
                i   += 1;
            }

            // Null terminator
            if (off - base_off) < arr_len * elem_sz {
                let tmp = self.regs.alloc(Span::POISONED)?;
                self.code.xor_rr(tmp, tmp);
                self.code.mov_store8(Reg::Rbp, off, tmp);
                self.regs.free(tmp);
            }

            return Ok(bytes.len() as u32 + 1); // \0
        }

        // Brace initializer: int arr[N] = {1, 2, 3}
        self.expect(TK::LCurly, "'{'")?;

        let mut idx = 0i32;
        while self.current_token.kind != TK::RCurly && !self.at_eof() {
            if idx >= arr_len {
                // Too many initializers, @Error out..?
                self.compile_expr_no_comma()?;
                self.vstack.pop();
            } else {
                let off = base_off + idx * elem_sz;
                self.compile_expr_no_comma()?;
                self.emit_store(Reg::Rbp, off, elem_ty)?;
            }

            idx += 1;
            if self.current_token.kind == TK::Comma { self.next(); }
        }

        self.expect(TK::RCurly, "'}'")?;

        // Zero pad remaining if sized
        if !inferred && idx < arr_len {  // @CodeOptimization: Use memset here?
            let tmp = self.regs.alloc(Span::POISONED)?;
            self.code.xor_rr(tmp, tmp);
            for i in idx..arr_len {
                let off = base_off + i * elem_sz;
                self.emit_int_store(Reg::Rbp, off, tmp, elem_ty);
            }
            self.regs.free(tmp);
        }

        Ok(idx as _)
    }

    #[inline]
    fn compile_inferred_array_length_local_decl(&mut self, ty: TypeRef, name_tok: Token) -> CResult<()> {
        if self.current_token.kind == TK::StrLit
            && self.types.get_kind(ty) == TypeKind::Char
        {
            //
            // Initialized global array of bytes (string)
            //

            // Count length including null terminator
            let t   = self.current_token;
            let raw = t.s(&self.pp.src_arena);
            let s   = &raw[1..raw.len()-1];
            let len = Self::unescape_len(s) + 1;

            let real_ty = self.types.array_of(ty, len as u32);
            let off = self.locals.alloc(name_tok.hash, real_ty, &self.types);
            self.compile_array_initializer(off, real_ty)?;

            return Ok(());
        }

        // Collect initializer tokens to count and replay
        let (elem_count, toks) = self.collect_tokens_inside_curlies()?;

        let real_ty = self.types.array_of(ty, elem_count);
        let off = self.locals.alloc(name_tok.hash, real_ty, &self.types);

        //
        // Replay tokens and compile initializer
        //

        let saved_cur  = self.pp.current_token;
        let saved_peek = self.pp.next_token;

        let mut replay = toks;
        replay.push(saved_cur);
        replay.push(saved_peek);

        self.pp.exp.push(replay.as_slice());
        self.pp.current_token = self.pp.cook();
        self.pp.next_token    = self.pp.cook();

        self.compile_array_initializer(off, real_ty)?;

        Ok(())
    }

    #[inline]
    fn compute_vla_flat_size(&mut self, ty: TypeRef, span: Span) -> CResult<Reg> {
        let r = self.regs.alloc(span)?;
        self.code.mov_ri64(r, 1);
        self.accumulate_vla_size(ty, r, span)?;
        Ok(r)
    }

    #[inline]
    fn accumulate_vla_size(&mut self, ty: TypeRef, acc: Reg, span: Span) -> CResult<()> {
        if self.types.get_kind(ty) != TypeKind::Array {
            let sz = self.types.size_of(ty) as i32;
            if sz > 1 { self.code.imul_ri(acc, sz); }
            return Ok(());
        }

        let e    = *self.types.get(ty);
        let elem = e.elem();
        if e.is_vla() {
            //
            // Runtime dim - load from hidden local and multiply
            //

            let tmp = self.regs.alloc(span)?;
            self.code.mov_load(tmp, Reg::Rbp, e.vla_dim_off(), true);
            self.code.imul_rr(acc, tmp);
            self.regs.free(tmp);
        } else {
            //
            // Comptime dim
            //

            self.code.imul_ri(acc, e.array_len() as i32);
        }

        self.accumulate_vla_size(elem, acc, span)
    }

    #[inline]
    fn finish_vla_local_decl(&mut self, outermost_ty: TypeRef, name_tok: Token) -> CResult<()> {
        if self.current_token.kind == TK::Eq {
            return Err(CError::VlaWithInitializer { span: self.current_token.span });
        }

        //
        // Compute total size
        //
        let total_r = self.regs.alloc(name_tok.span)?;
        self.code.mov_ri64(total_r, 1);   // So we don't multiply 0
        self.accumulate_vla_size(outermost_ty, total_r, name_tok.span)?;

        //
        // Cache total size
        //
        let size_off = self.locals.alloc_vla(HASH_HIDDEN_LOCAL, TYPE_LONG);
        self.code.mov_store(Reg::Rbp, size_off, total_r, true);
        self.vla_sizes.insert(outermost_ty, size_off);

        //
        // Align to 16 (SYSV) and allocate on stack
        //
        self.code.add_ri8(total_r, 15);
        self.code.and_ri(total_r, -16);
        self.code.sub_rr(Reg::Rsp, total_r);
        self.regs.free(total_r);

        //
        // Named local: stores VLA array type, rbp_off = base pointer slot
        //
        let ptr_off = self.locals.alloc_vla(name_tok.hash, outermost_ty);
        let array_base = Reg::Rsp;
        self.code.mov_store(Reg::Rbp, ptr_off, array_base, true);

        Ok(())
    }

    #[inline]
    fn compile_local_decl(&mut self) -> CResult<()> {
        let base_ty = self.compile_base_type()?;
        loop {
            self.compile_local_decl_impl(base_ty)?;
            if self.current_token.kind != TK::Comma { break; }

            self.next(); // ,
        }

        self.expect(TK::SemiColon, "';'").map(|_| ())
    }

    #[inline]
    fn compile_local_decl_impl(&mut self, base_ty: TypeRef) -> CResult<()> {
        let (ty, name_tok) = self.parse_declarator(base_ty)?;
        let name_tok = name_tok.ok_or_else(|| CError::Expected {
            span: self.current_token.span,
            expected: "variable name",
            got: self.s(self.current_token).to_owned(),
        })?;

        // Inferred array: int arr[] = {...}
        if self.types.get_kind(ty) == TypeKind::Array
            && self.types.get(ty).array_len() == 0
            && self.current_token.kind == TK::Eq
        {
            self.next(); // =
            return self.compile_inferred_array_length_local_decl(
                self.types.get(ty).elem(), name_tok
            );
        }

        if self.types.get(ty).is_vla() {
            return self.finish_vla_local_decl(ty, name_tok);
        }

        let off = self.locals.alloc(name_tok.hash, ty, &self.types);
        if self.current_token.kind == TK::Eq {
            self.next(); // =

            //
            // Fast path: scalar with no braces
            //
            let kind = self.get_kind(ty);
            if !matches!(kind, TypeKind::Struct | TypeKind::Union | TypeKind::Array)
                && self.current_token.kind != TK::LCurly
            {
                self.compile_expr_no_comma()?;
                self.emit_store(Reg::Rbp, off, ty)?;
                return Ok(());
            }

            self.compile_initializer(off, ty)?;
        }

        Ok(())
    }

    #[inline]
    fn compile_expr_stmt(&mut self) -> CResult<()> {
        self.compile_expr()?;
        let v = self.vstack.pop();
        if matches!(v.kind(), VK::Reg | VK::RegInd) {
            self.free_reg(v.reg());
        }
        self.expect(TK::SemiColon, "';'").map(|_| ())
    }

    #[inline]
    const fn op_prec(k: TK) -> Option<(u8, bool)> {
        match k {
            TK::Comma => Some((0, false)),

            TK::Eq     | TK::PlusEq  | TK::MinusEq  |
            TK::StarEq | TK::SlashEq | TK::BinAndEq |
            TK::XorEq  | TK::BinOrEq => Some((2, true)),
            TK::Or                   => Some((3, false)),
            TK::And                  => Some((4, false)),
            TK::BinOr                => Some((5, false)),
            TK::Xor                  => Some((6, false)),
            TK::BinAnd               => Some((7, false)),
            TK::EqEq | TK::NotEq     => Some((8, false)),
            TK::Less | TK::Greater | TK::LessEq | TK::GreaterEq => Some((9, false)),
            TK::Plus | TK::Minus     => Some((10, false)),
            TK::Star | TK::Slash     => Some((11, false)),

            _ => None
        }
    }

    #[inline]
    fn compile_expr(&mut self) -> CResult<()> {
        self.compile_expr_impl(0)
    }

    #[inline]
    fn compile_expr_no_comma(&mut self) -> CResult<()> {
        self.compile_expr_impl(1)  // stops before comma
    }

    #[inline]
    fn compile_expr_impl(&mut self, min_prec: u8) -> CResult<()> {
        match self.current_token.kind {
            TK::Minus | TK::BinAnd | TK::Not | TK::Star | TK::BitNot | TK::PlusPlus | TK::MinusMinus
                => self.compile_unary()?,
            _   => self.compile_primary()?,
        }

        loop {
            let (prec, right) = match Self::op_prec(self.current_token.kind) {
                Some(p) if p.0 >= min_prec => p,
                _ => break,
            };

            let op   = self.current_token.kind;
            let span = self.current_token.span;
            self.next();

            match op {
                // Compound assignment operators
                TK::Eq | TK::PlusEq | TK::MinusEq |
                TK::StarEq | TK::SlashEq | TK::BinAndEq |
                TK::XorEq  | TK::BinOrEq => {
                    self.compile_assign(op, prec, right, span)?;
                }

                // Short circuit
                TK::And => self.compile_logical_and(prec, span)?,
                TK::Or  => self.compile_logical_or(prec, span)?,

                _ => {
                    self.compile_expr_impl(if right { prec } else { prec + 1 })?;
                    self.compile_binop(op, span)?;
                }
            }
        }

        Ok(())
    }

    #[inline]
    fn compile_assign(&mut self, op: TK, prec: u8, right: bool, span: Span) -> CResult<()> {
        let lhs = self.vstack.pop();
        if !lhs.is_lvalue() { return Err(CError::NotLvalue { span }); }

        //
        // Spill lhs base address if it's a RegInd - rhs evaluation (e.g. function calls)
        // can clobber the base register before we get to emit the store.
        //
        let (lhs, lhs_addr_spill) = if lhs.kind() == VK::RegInd && lhs.reg().as_gp() != Reg::Rbp {
            let addr_off = self.locals.alloc(HASH_HIDDEN_LOCAL, TYPE_LONG, &self.types);
            let base = lhs.reg().as_gp();
            self.code.mov_store(Reg::Rbp, addr_off, base, true);
            self.regs.free(base);
            (CValue::regind(lhs.ty, Reg::Rbp, addr_off), Some(addr_off))
        } else {
            (lhs, None)
        };

        self.compile_expr_impl(if right { prec } else { prec + 1 })?;

        //
        // Reload lhs base address if we spilled it
        //
        let lhs = if let Some(addr_off) = lhs_addr_spill {
            let r = self.regs.alloc(span)?;
            self.code.mov_load(r, Reg::Rbp, addr_off, true);
            CValue::regind(lhs.ty, r, 0)
        } else {
            lhs
        };

        if op != TK::Eq {
            //
            // Compound assignment path: +=, /=, etc
            //

            // Load lhs current value, push it, push rhs, do the op
            let rhs = self.vstack.pop();
            let base = lhs.reg().as_gp();

            // @Refactor: float/int handling
            if self.is_float(lhs.ty) {
                let tmp = self.xmms.alloc(span)?;
                self.emit_float_load(tmp, base, lhs.offset(), lhs.ty);
                self.vstack.push(CValue::xmm(lhs.ty, tmp));
            } else {
                let tmp = self.regs.alloc(span)?;
                self.emit_int_load(tmp, base, lhs.offset(), lhs.ty);
                self.vstack.push(CValue::gp(lhs.ty, tmp));
            }

            self.vstack.push(rhs);
            self.compile_binop(op.compound_to_binop(), span)?;
        }

        let base = lhs.reg().as_gp();
        self.emit_store_keep(base, lhs.offset(), lhs.ty)?;
        self.regs.free(base);

        Ok(())
    }

    #[inline]
    fn compile_logical_and(&mut self, prec: u8, span: Span) -> CResult<()> {
        let (l, _) = self.pop_reg()?;
        self.code.test_rr(l);
        self.regs.free(l);

        let short = self.code.je_rel32();  // Skip if lhs false

        self.compile_expr_impl(prec + 1)?;
        let (r, _) = self.pop_reg()?;
        self.code.test_rr(r);
        self.regs.free(r);

        let result = self.regs.alloc(span)?;
        self.code.setcc(result, 0x95); // setne
        self.code.movzx_rr(result, result);

        let done = self.code.jmp_rel32();
        self.code.patch_rel32(short, self.code.pos());
        self.code.xor_rr(result, result); // false
        self.code.patch_rel32(done, self.code.pos());

        self.vstack.push(CValue::gp(TYPE_INT, result));
        Ok(())
    }

    #[inline]
    fn compile_logical_or(&mut self, prec: u8, span: Span) -> CResult<()> {
        // @Cutnpaste from compile_logical_and

        let (l, _) = self.pop_reg()?;
        self.code.test_rr(l);
        self.regs.free(l);

        let short = self.code.jne_rel32();  // Skip if lhs true

        self.compile_expr_impl(prec + 1)?;
        let (r, _) = self.pop_reg()?;
        self.code.test_rr(r);
        self.regs.free(r);

        let result = self.regs.alloc(span)?;
        self.code.setcc(result, 0x95); // setne
        self.code.movzx_rr(result, result);

        let done = self.code.jmp_rel32();
        self.code.patch_rel32(short, self.code.pos());
        self.code.mov_ri64(result, 1); // true
        self.code.patch_rel32(done, self.code.pos());

        self.vstack.push(CValue::gp(TYPE_INT, result));
        Ok(())
    }

    #[inline]
    fn decay_array_type(&mut self, ty: TypeRef) -> TypeRef {
        if self.types.get_kind(ty) != TypeKind::Array {
            return ty;
        }

        let arr     = self.types.get(ty);
        let elem_ty = arr.elem();

        //
        // Array-level quals (const/volatile) transfer to the pointee
        // while restrict is dropped.
        //
        let inherited = arr.quals - QualFlags::RESTRICT;
        let elem_ty = if inherited.is_empty() {
            elem_ty
        } else {
            // Merge with any quals already on the element type
            let existing = self.types.get(elem_ty).quals;
            self.types.qualify(elem_ty, existing | inherited)
        };

        let ptr_ty = self.types.ptr_to(elem_ty);
        self.types.associate_type_name(ty, ptr_ty);

        ptr_ty
    }

    #[inline]
    fn decay_array(&mut self, v: CValue) -> CResult<CValue> {
        if self.types.get_kind(v.ty) != TypeKind::Array {
            return Ok(v);
        }

        if self.types.get(v.ty).is_vla() {
            // rbp_off holds pointer to array base - load it
            let r = self.regs.alloc(Span::POISONED)?;
            self.code.mov_load(r, Reg::Rbp, v.offset(), true);
            let elem_ty = self.types.get(v.ty).elem();
            let ptr_ty  = self.types.ptr_to(elem_ty);
            return Ok(CValue::gp(ptr_ty, r));
        }


        let ptr_ty = self.decay_array_type(v.ty);

        match v.kind() {
            VK::Local | VK::RegInd => {
                // Just load the effective address into a register and return it.

                let base = v.reg().as_gp();
                let r = self.regs.alloc(Span::POISONED)?;
                self.code.lea(r, base, v.offset());
                if v.kind() == VK::RegInd { self.regs.free(base); }
                Ok(CValue::gp(ptr_ty, r))
            }

            VK::Reg => {
                // Already a register holding the address
                Ok(CValue { ty: ptr_ty, ..v })
            }

            VK::Imm => Ok(CValue { ty: ptr_ty, ..v }),
        }
    }

    #[inline]
    fn pop_vstack_and_decay_array(&mut self) -> CResult<CValue> {
        let v = self.vstack.pop();
        self.decay_array(v)
    }

    #[inline]
    fn compile_binop(&mut self, op: TK, span: Span) -> CResult<()> {
        match op {
            TK::Plus | TK::Minus                                 => self.compile_additive(op, span)?,
            TK::Star | TK::Slash                                 => self.compile_multiplicative(op, span)?,
            TK::BinAnd | TK::BinOr | TK::Xor                     => self.compile_bitwise(op)?,
            TK::EqEq | TK::NotEq |
            TK::Less | TK::Greater | TK::LessEq | TK::GreaterEq  => self.compile_cmp(op)?,

            TK::Comma => {
                // Discard lhs, keep rhs - already on vstack in right order
                let rhs = self.vstack.pop();
                let lhs = self.vstack.pop();
                if matches!(lhs.kind(), VK::Reg | VK::RegInd) { self.free_reg(lhs.reg()); }
                self.vstack.push(rhs);
            }

            other => unreachable!("{other:?}"),
        }

        Ok(())
    }

    #[inline]
    fn compile_additive(&mut self, op: TK, span: Span) -> CResult<()> {
        let vrhs = self.pop_vstack_and_decay_array()?;
        let vlhs = self.pop_vstack_and_decay_array()?;

        if vlhs.kind() == VK::Imm && vrhs.kind() == VK::Imm {
            // Constant folding
            let common_ty = self.get_usual_arithmetic_conversion_type(vlhs.ty, vrhs.ty);
            let result = match op {
                TK::Plus  => vlhs.imm.wrapping_add(vrhs.imm),
                TK::Minus => vlhs.imm.wrapping_sub(vrhs.imm),
                _ => { return self.compile_additive_impl(op, span, vlhs, vrhs); }
            };
            self.vstack.push(CValue::imm(common_ty, result));
            return Ok(());
        }

        self.compile_additive_impl(op, span, vlhs, vrhs)
    }

    #[inline]
    fn compile_additive_impl(&mut self, op: TK, _span: Span, vlhs: CValue, vrhs: CValue) -> CResult<()> {
        // Float path
        if self.is_float(vlhs.ty) || self.is_float(vrhs.ty) {
            return self.compile_float_binop(op, vlhs, vrhs);
        }

        // Pointer arithmetic path
        if op == TK::Plus && (self.is_ptr(vlhs.ty) || self.is_ptr(vrhs.ty)) {
            return self.compile_ptr_add(vlhs, vrhs);
        }
        if op == TK::Minus && self.is_ptr(vlhs.ty) {
            return self.compile_ptr_sub(vlhs, vrhs);
        }

        // Integer path
        let (vlhs, vrhs, common_ty) = self.do_usual_arithmetic_conversion(vlhs, vrhs)?;
        let lhs = self.force_gp(vlhs)?;
        let rhs = self.force_gp(vrhs)?;
        match op {
            TK::Plus  => self.code.add_rr(lhs, rhs),
            TK::Minus => self.code.sub_rr(lhs, rhs),
            _ => unreachable!(),
        }

        self.regs.free(rhs);
        self.vstack.push(CValue::gp(common_ty, lhs));

        Ok(())
    }

    #[inline]
    fn compile_multiplicative_impl(&mut self, op: TK, span: Span, vlhs: CValue, vrhs: CValue) -> CResult<()> {
        // Float path
        if self.is_float(vlhs.ty) || self.is_float(vrhs.ty) {
            return self.compile_float_binop(op, vlhs, vrhs);
        }

        let (vlhs, vrhs, common_ty) = self.do_usual_arithmetic_conversion(vlhs, vrhs)?;
        let lhs = self.force_gp(vlhs)?;
        let rhs = self.force_gp(vrhs)?;

        match op {
            TK::Star  => {
                self.code.imul_rr(lhs, rhs);
                self.regs.free(rhs);
                self.vstack.push(CValue::gp(common_ty, lhs));
            }

            TK::Slash => {
                self.code.mov_rr(Reg::Rax, lhs);
                self.code.cqo();
                let actual = if rhs == Reg::Rdx {
                    let t = self.regs.alloc(span)?;
                    self.code.mov_rr(t, Reg::Rdx);
                    t
                } else {
                    rhs
                };

                self.code.idiv_r(actual);
                self.regs.free(lhs);
                self.regs.free(actual);

                if actual != rhs { self.regs.free(rhs); }
                self.regs.mark(Reg::Rax);
                self.vstack.push(CValue::gp(common_ty, Reg::Rax));
            }

            _ => unreachable!(),
        }

        Ok(())
    }

    #[inline]
    fn compile_multiplicative(&mut self, op: TK, span: Span) -> CResult<()> {
        let vrhs = self.vstack.pop();
        let vlhs = self.vstack.pop();

        if vlhs.kind() == VK::Imm && vrhs.kind() == VK::Imm && !self.is_float(vlhs.ty) {
            let common_ty = self.get_usual_arithmetic_conversion_type(vlhs.ty, vrhs.ty);
            let result = match op {
                TK::Star  => vlhs.imm.wrapping_mul(vrhs.imm),
                TK::Slash => if vrhs.imm != 0 { vlhs.imm / vrhs.imm } else { 0 },
                _ => unreachable!(),
            };
            self.vstack.push(CValue::imm(common_ty, result));
            return Ok(());
        }

        self.compile_multiplicative_impl(op, span, vlhs, vrhs)
    }

    #[inline]
    fn compile_bitwise(&mut self, op: TK) -> CResult<()> {
        let rhs = self.vstack.pop();
        let lhs = self.vstack.pop();

        if lhs.kind() == VK::Imm && rhs.kind() == VK::Imm {
            let common_ty = self.get_usual_arithmetic_conversion_type(lhs.ty, rhs.ty);
            let result = match op {
                TK::BinAnd => lhs.imm & rhs.imm,
                TK::BinOr  => lhs.imm | rhs.imm,
                TK::Xor    => lhs.imm ^ rhs.imm,
                _ => unreachable!(),
            };
            self.vstack.push(CValue::imm(common_ty, result));
            return Ok(());
        }

        let (lhs, rhs, common_ty) = self.do_usual_arithmetic_conversion(lhs, rhs)?;
        let rhs = self.force_gp(rhs)?;
        let lhs = self.force_gp(lhs)?;

        match op {
            TK::BinAnd => self.code.and_rr(lhs, rhs),
            TK::BinOr  => self.code.or_rr(lhs, rhs),
            TK::Xor    => self.code.xor_rr(lhs, rhs),
            _ => unreachable!(),
        }

        self.regs.free(rhs);
        self.vstack.push(CValue::gp(common_ty, lhs));

        Ok(())
    }

    #[inline]
    fn compile_float_binop(&mut self, op: TK, vlhs: CValue, vrhs: CValue) -> CResult<()> {
        let target_ty = if self.get_kind(vlhs.ty) == TypeKind::Double
                        || self.get_kind(vrhs.ty) == TypeKind::Double
        { TYPE_DOUBLE } else { TYPE_FLOAT };

        let l = self.coerce_to_xmm(vlhs, target_ty)?;
        let r = self.coerce_to_xmm(vrhs, target_ty)?;
        let ty = self.normalize_xmm(l, vlhs.ty, r, vrhs.ty);

        self.emit_float_arith(op, l, r, ty);

        self.xmms.free(r);
        self.vstack.push(CValue::xmm(ty, l));

        Ok(())
    }

    #[inline]
    fn compile_ptr_add(&mut self, vlhs: CValue, vrhs: CValue) -> CResult<()> {
        let (ptr_val, idx_val, ptr_ty) = if self.is_ptr(vlhs.ty) {
            (vlhs, vrhs, vlhs.ty)
        } else {
            (vrhs, vlhs, vrhs.ty)
        };

        let elem_ty = self.types.deref(ptr_ty);
        let elem_sz = self.types.size_of(elem_ty) as i32;

        let base  = self.force_gp(ptr_val)?;
        let idx_val = self.integer_promote(idx_val)?;
        let idx_r   = self.force_gp(idx_val)?;

        self.scale_index(idx_r, elem_sz);
        self.code.add_rr(base, idx_r);
        self.regs.free(idx_r);
        self.vstack.push(CValue::gp(ptr_ty, base));
        Ok(())
    }

    #[inline]
    fn compile_ptr_sub(&mut self, vlhs: CValue, vrhs: CValue) -> CResult<()> {
        let elem_ty = self.types.deref(vlhs.ty);
        let elem_sz = self.types.size_of(elem_ty) as i32;

        if self.is_ptr(vrhs.ty) {
            // ptr - ptr = ptrdiff
            let l = self.force_gp(vlhs)?;
            let r = self.force_gp(vrhs)?;
            self.code.sub_rr(l, r);
            self.regs.free(r);
            if elem_sz > 1 {
                // l = l / elem_sz

                self.code.mov_rr(Reg::Rax, l);
                self.code.cqo();
                self.code.mov_ri64(Reg::Rcx, elem_sz as i64);
                self.code.idiv_r(Reg::Rcx);
                self.regs.free(l);
                self.regs.mark(Reg::Rax);
                self.vstack.push(CValue::gp(TYPE_LONG, Reg::Rax));
            } else {
                self.vstack.push(CValue::gp(TYPE_LONG, l));
            }
        } else {
            // ptr - int
            let base  = self.force_gp(vlhs)?;
            let vrhs  = self.integer_promote(vrhs)?;
            let idx_r = self.force_gp(vrhs)?;
            self.scale_index(idx_r, elem_sz);
            self.code.sub_rr(base, idx_r);
            self.regs.free(idx_r);
            self.vstack.push(CValue::gp(vlhs.ty, base));
        }

        Ok(())
    }

    // Scale idx_r by elem_sz in place - uses imul_ri to avoid clobbering Rcx
    #[inline]
    fn scale_index(&mut self, idx_r: Reg, elem_sz: i32) {
        match elem_sz {
            1 => {}
            2 => { self.code.add_rr(idx_r, idx_r); }
            _ => { self.code.imul_ri(idx_r, elem_sz); }
        }
    }

    // Promote float->double if types differ. Returns the common type.
    #[inline]
    fn normalize_xmm(&mut self, l: XmmReg, lty: TypeRef, r: XmmReg, rty: TypeRef) -> TypeRef {
        if lty == rty { return lty; }

        // One is float, one is double - promote float to double
        if self.get_kind(lty) == TypeKind::Float { self.code.cvtss2sd(l, l); }
        if self.get_kind(rty) == TypeKind::Float { self.code.cvtss2sd(r, r); }

        TYPE_DOUBLE
    }

    #[inline]
    fn compile_cmp(&mut self, op: TK) -> CResult<()> {
        let rhs = self.vstack.pop();
        let lhs = self.vstack.pop();

        if lhs.kind() == VK::Imm && rhs.kind() == VK::Imm && !self.is_float(lhs.ty) {
            //
            // Apply UAC to get correct signedness for the comparison
            //

            let common_ty  = self.get_usual_arithmetic_conversion_type(lhs.ty, rhs.ty);
            let unsigned   = self.types.is_unsigned(common_ty);

            let result = if unsigned {
                let l = lhs.imm as u64;
                let r = rhs.imm as u64;
                match op {
                    TK::EqEq      => (l == r) as i64,
                    TK::NotEq     => (l != r) as i64,
                    TK::Less      => (l <  r) as i64,
                    TK::Greater   => (l >  r) as i64,
                    TK::LessEq    => (l <= r) as i64,
                    TK::GreaterEq => (l >= r) as i64,
                    _ => unreachable!(),
                }
            } else {
                match op {
                    TK::EqEq      => (lhs.imm == rhs.imm) as i64,
                    TK::NotEq     => (lhs.imm != rhs.imm) as i64,
                    TK::Less      => (lhs.imm <  rhs.imm) as i64,
                    TK::Greater   => (lhs.imm >  rhs.imm) as i64,
                    TK::LessEq    => (lhs.imm <= rhs.imm) as i64,
                    TK::GreaterEq => (lhs.imm >= rhs.imm) as i64,
                    _ => unreachable!(),
                }
            };

            self.vstack.push(CValue::imm(TYPE_INT, result));
            return Ok(());
        }

        let rhs = self.decay_array(rhs)?;
        let lhs = self.decay_array(lhs)?;

        let lhs_ty = self.get_kind(lhs.ty);
        let rhs_ty = self.get_kind(rhs.ty);

        if self.is_float(lhs.ty) || self.is_float(rhs.ty) {
            let target_ty = if lhs_ty == TypeKind::Double || rhs_ty == TypeKind::Double {
                TYPE_DOUBLE
            } else {
                TYPE_FLOAT
            };
            let l = self.coerce_to_xmm(lhs, target_ty)?;
            let r = self.coerce_to_xmm(rhs, target_ty)?;

            self.emit_float_cmp(l, r, target_ty);

            self.xmms.free(l);
            self.xmms.free(r);

            let dst = self.regs.alloc(Span::POISONED)?;
            let setcc: u8 = match op {
                TK::EqEq      => 0x94, // sete
                TK::NotEq     => 0x95, // setne
                TK::Less      => 0x92, // setb
                TK::LessEq    => 0x96, // setbe
                TK::Greater   => 0x97, // seta
                TK::GreaterEq => 0x93, // setae
                _ => unreachable!(),
            };
            self.code.setcc(dst, setcc);

            self.code.movzx_rr(dst, dst);
            self.vstack.push(CValue::gp(TYPE_INT, dst));
        } else {
            let (lhs, rhs, _) = self.do_usual_arithmetic_conversion(lhs, rhs)?;
            let r = self.force_gp(rhs)?;
            let l = self.force_gp(lhs)?;
            self.code.cmp_rr(l, r);

            let setcc: u8 = match op {
                TK::EqEq      => 0x94,
                TK::NotEq     => 0x95,
                TK::Less      => 0x9C,
                TK::Greater   => 0x9F,
                TK::LessEq    => 0x9E,
                TK::GreaterEq => 0x9D,
                _ => unreachable!(),
            };
            self.code.setcc(l, setcc);
            self.code.movzx_rr(l, l);

            self.regs.free(r);
            self.vstack.push(CValue::gp(TYPE_INT, l));
        }

        Ok(())
    }

    #[inline]
    fn compile_unary(&mut self) -> CResult<()> {
        match self.current_token.kind {
            TK::Minus => {
                self.next();
                self.compile_unary()?;

                let v = self.vstack.peek();
                if !self.is_float(v.ty) {
                    let (r, ty) = self.pop_reg()?;
                    self.code.neg_r(r);
                    self.vstack.push(CValue::gp(ty, r));
                    return Ok(());
                }

                let (r, ty) = self.pop_xmm()?;

                // xorpd xmm, [rip + sign_mask] - flip sign bit
                let rodata_off = self.rodata.len() as u32;
                match self.get_kind(ty) {
                    TypeKind::Float => self.rodata.extend_from_slice(&0x80000000u32.to_le_bytes()),
                    _               => self.rodata.extend_from_slice(&0x8000000000000000u64.to_le_bytes()),
                }

                let text_off = self.emit_float_xor_rip(r, ty) as _;

                self.rodata_relocs.push(RodataReloc { text_off, rodata_off });
                self.vstack.push(CValue::xmm(ty, r));
            }

            TK::BinAnd => {
                let span = self.current_token.span; self.next();
                self.compile_primary()?;

                let v = self.vstack.pop();
                if !v.is_lvalue() { return Err(CError::NotLvalue { span }); }

                let base = v.reg().as_gp();

                let dst = self.regs.alloc(span)?;
                self.code.lea(dst, base, v.offset());
                if v.kind() == VK::RegInd { self.regs.free(base); }

                let new_type = self.types.ptr_to(v.ty);
                self.vstack.push(CValue::gp(new_type, dst));
            }

            TK::BitNot => {
                self.next();
                self.compile_unary()?;

                let (r, ty) = self.pop_reg()?;
                self.code.not_r(r);
                self.vstack.push(CValue::gp(ty, r));
            }

            TK::Not => {
                self.next();
                self.compile_unary()?;
                let (r, _) = self.pop_reg()?;
                self.code.test_rr(r);
                self.regs.free(r);
                let dst = self.regs.alloc(Span::POISONED)?;
                self.code.setcc(dst, 0x94); // sete - true if zero
                self.code.movzx_rr(dst, dst);
                self.vstack.push(CValue::gp(TYPE_INT, dst));
            }

            TK::PlusPlus | TK::MinusMinus => {
                let op = self.current_token.kind;
                self.next();
                self.compile_unary()?;

                let v = self.vstack.pop();
                if !v.is_lvalue() { return Err(CError::NotLvalue { span: Span::POISONED }); }

                let base = v.reg().as_gp();
                let r = self.regs.alloc(Span::POISONED)?;

                let promoted_ty = self.promoted_type(v.ty); // see helper below
                self.emit_int_load(r, base, v.offset(), v.ty);

                // Extend to promoted width before arithmetic
                let from_size = self.types.size_of(v.ty);
                let is_signed = self.types.is_signed(v.ty);
                self.emit_extend(r, from_size, self.types.size_of(promoted_ty), is_signed);

                match op {
                    TK::PlusPlus  => self.code.add_ri8(r, 1),
                    _             => self.code.add_ri8(r, -1),
                }

                // Store back as original narrow type (truncation = implicit)
                self.emit_int_store(base, v.offset(), r, v.ty);

                if v.kind() == VK::RegInd { self.regs.free(base); }
                self.vstack.push(CValue::gp(v.ty, r));
            }

            TK::Star => {
                let star = self.next();
                self.compile_unary()?;

                let v = self.pop_vstack_and_decay_array()?;
                if !(self.is_ptr(v.ty) || self.is_array(v.ty)) {
                    return Err(CError::Expected {
                        span: star.span,
                        expected: "array or pointer",
                        got: self.types.to_string(v.ty)
                    })
                }

                let r  = self.force_gp(v)?;
                let ty = self.types.deref(v.ty);

                self.vstack.push(CValue::regind(ty, r, 0));
            }

            _ => self.compile_primary()?,
        }

        Ok(())
    }

    #[inline]
    fn emit_int_load(&mut self, dst: Reg, base: Reg, off: i32, ty: TypeRef) {
        let unsigned = self.types.is_unsigned(ty);
        match self.types.size_of(ty) {
            1 => if unsigned { self.code.movzx8_load(dst, base, off) }
                 else        { self.code.movsx8_load(dst, base, off) },
            2 => if unsigned { self.code.movzx16_load(dst, base, off) }
                 else        { self.code.movsx16_load(dst, base, off) },
            4 => self.code.mov_load(dst, base, off, false),
            _ => self.code.mov_load(dst, base, off, true),
        }
    }

    #[inline]
    fn emit_int_store(&mut self, base: Reg, off: i32, src: Reg, ty: TypeRef) {
        match self.types.size_of(ty) {
            1 => self.code.mov_store8(base, off, src),
            2 => self.code.mov_store16(base, off, src),
            4 => self.code.mov_store(base, off, src, false),
            _ => self.code.mov_store(base, off, src, true),
        }
    }

    #[inline]
    fn emit_float_load(&mut self, dst: XmmReg, base: Reg, off: i32, ty: TypeRef) {
        match self.get_kind(ty) {
            TypeKind::Float => self.code.movss_load(dst, base, off),
            _               => self.code.movsd_load(dst, base, off),
        }
    }

    #[inline]
    fn emit_float_store(&mut self, base: Reg, off: i32, src: XmmReg, ty: TypeRef) {
        match self.get_kind(ty) {
            TypeKind::Float => self.code.movss_store(base, off, src),
            _            => self.code.movsd_store(base, off, src),
        }
    }

    #[inline]
    fn emit_float_mov(&mut self, dst: XmmReg, src: XmmReg, ty: TypeRef) {
        if dst == src { return; }
        match self.get_kind(ty) {
            TypeKind::Float => self.code.movss_rr(dst, src),
            _            => self.code.movsd_rr(dst, src),
        }
    }

    #[inline]
    fn emit_float_load_rip(&mut self, dst: XmmReg, ty: TypeRef) -> usize {
        match self.get_kind(ty) {
            TypeKind::Float => self.code.movss_load_rip(dst),
            _            => self.code.movsd_load_rip(dst),
        }
    }

    #[inline]
    fn emit_float_xor_rip(&mut self, dst: XmmReg, ty: TypeRef) -> usize {
        match self.get_kind(ty) {
            TypeKind::Float => self.code.xorps_rip(dst),
            _            => self.code.xorpd_rip(dst),
        }
    }

    #[inline]
    fn emit_float_arith(&mut self, op: TK, dst: XmmReg, src: XmmReg, ty: TypeRef) {
        match (op, self.get_kind(ty)) {
            (TK::Plus,  TypeKind::Float) => self.code.addss(dst, src),
            (TK::Plus,  _)               => self.code.addsd(dst, src),
            (TK::Minus, TypeKind::Float) => self.code.subss(dst, src),
            (TK::Minus, _)               => self.code.subsd(dst, src),
            (TK::Star,  TypeKind::Float) => self.code.mulss(dst, src),
            (TK::Star,  _)               => self.code.mulsd(dst, src),
            (TK::Slash, TypeKind::Float) => self.code.divss(dst, src),
            (TK::Slash, _)               => self.code.divsd(dst, src),
            _ => unreachable!()
        }
    }

    #[inline]
    fn emit_float_cmp(&mut self, lhs: XmmReg, rhs: XmmReg, ty: TypeRef) {
        match self.get_kind(ty) {
            TypeKind::Float => self.code.ucomiss(lhs, rhs),
            _               => self.code.ucomisd(lhs, rhs),
        }
    }

    #[inline]
    fn with_rollback<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let buf_pos           = self.code.pos();
        let rodata_pos        = self.rodata.len();
        let rodata_relocs_len = self.rodata_relocs.len();
        let data_relocs_len   = self.data_relocs.len();
        let vstack_top        = self.vstack.top;
        let regs_used         = self.regs.used;
        let xmms_used         = self.xmms.used;

        let result = f(self);

        self.code.bytes.truncate(buf_pos);
        self.rodata.truncate(rodata_pos);
        self.rodata_relocs.truncate(rodata_relocs_len);
        self.data_relocs.truncate(data_relocs_len);
        self.vstack.top = vstack_top;
        self.regs.used  = regs_used;
        self.xmms.used  = xmms_used;

        result
    }

    #[inline]
    fn try_eval_const_expr(&mut self) -> CResult<CValue> {
        self.with_rollback(|c| {
            c.compile_expr_no_comma()?;
            Ok(c.vstack.pop())
        })
    }

    #[inline]
    fn try_parse_and_eval_const_int(&mut self) -> CResult<i64> {
        let expr_start_span = self.current_token.span;
        let v = self.try_eval_const_expr()?;

        if matches!(v.kind(), VK::Imm) {
            Ok(v.imm)
        } else {
            return Err(CError::Expected {
                span: expr_start_span,
                expected: "constant integer expression",
                got: format!("{:?}", v.kind()),
            })
        }
    }

    #[inline]
    fn is_hash_a_local_or_a_global(&self, h: u64) -> bool {
        self.locals.find(h).is_some() || self.globals.find(h).is_some()
    }

    #[inline]
    fn try_parse_eval_const_float(&mut self) -> CResult<f64> {
        let rodata_before = self.rodata.len();

        self.with_rollback(|c| {
            c.compile_expr()?;
            let v = c.vstack.pop();

            Ok(match v.kind() {
                VK::Imm => v.get_fimm(),

                VK::Reg => {
                    let rodata_off = rodata_before;
                    match c.types.get_kind(v.ty) {
                        TypeKind::Float => {
                            let bytes = &c.rodata[rodata_off..rodata_off+4];
                            f32::from_bits(u32::from_le_bytes(bytes.try_into().unwrap())) as f64
                        }
                        _ => {
                            let bytes = &c.rodata[rodata_off..rodata_off+8];
                            f64::from_bits(u64::from_le_bytes(bytes.try_into().unwrap()))
                        }
                    }
                }

                _ => return Err(CError::Expected {
                    span: c.current_token.span,
                    expected: "constant float expression",
                    got: format!("{:?}", v.kind()),
                })
            })
        })
    }

    fn emit_cast(&mut self, v: CValue, to: TypeRef) -> CResult<()> {
        let from = v.ty;

        // Same type - no-op
        if from == to {
            self.vstack.push(v);
            return Ok(());
        }

        let from_kind = self.get_kind(from);
        let to_kind   = self.get_kind(to);

        match (from_kind, to_kind) {
            // int -> int: sign/zero extend or truncate
            (TypeKind::Int | TypeKind::Char | TypeKind::Short | TypeKind::Long | TypeKind::LLong,
             TypeKind::Int | TypeKind::Char | TypeKind::Short | TypeKind::Long | TypeKind::LLong) => {
                let r = self.force_gp(v)?;
                let to_size   = self.types.size_of(to);
                let from_size = self.types.size_of(from);
                let is_signed = self.types.is_signed(from);
                if to_size > from_size {
                    self.emit_extend(r, from_size, to_size, is_signed);
                }
                // truncation is a no-op on x86-64, the upper bits just get ignored
                self.vstack.push(CValue::gp(to, r));
            }

            // int -> float
            (TypeKind::Int | TypeKind::Long | TypeKind::LLong,
             TypeKind::Float | TypeKind::Double) => {
                let r = self.force_gp(v)?;
                let xmm = self.xmms.alloc(Span::POISONED)?;
                if to_kind == TypeKind::Float {
                    self.code.cvtsi2ss(xmm, r);
                } else {
                    self.code.cvtsi2sd(xmm, r);
                }
                self.regs.free(r);
                self.vstack.push(CValue::xmm(to, xmm));
            }

            // float -> int
            (TypeKind::Float | TypeKind::Double,
             TypeKind::Int | TypeKind::Long | TypeKind::LLong) => {
                let xmm = self.force_xmm(v)?;
                let r = self.regs.alloc(Span::POISONED)?;
                if from_kind == TypeKind::Float {
                    self.code.cvttss2si(r, xmm);
                } else {
                    self.code.cvttsd2si(r, xmm);
                }
                self.xmms.free(xmm);
                self.vstack.push(CValue::gp(to, r));
            }

            // float <-> float
            (TypeKind::Float, TypeKind::Double) => {
                let xmm = self.force_xmm(v)?;
                self.code.cvtss2sd(xmm, xmm);
                self.vstack.push(CValue::xmm(to, xmm));
            }
            (TypeKind::Double, TypeKind::Float) => {
                let xmm = self.force_xmm(v)?;
                self.code.cvtsd2ss(xmm, xmm);
                self.vstack.push(CValue::xmm(to, xmm));
            }

            // ptr -> ptr, ptr -> int, int -> ptr: just retag
            (TypeKind::Ptr, TypeKind::Ptr) |
            (TypeKind::Ptr, TypeKind::Int) | (TypeKind::Ptr, TypeKind::Long) | (TypeKind::Ptr, TypeKind::LLong) |
            (TypeKind::Int, TypeKind::Ptr) | (TypeKind::Long, TypeKind::Ptr) | (TypeKind::LLong, TypeKind::Ptr) => {
                let r = self.force_gp(v)?;
                self.vstack.push(CValue::gp(to, r));
            }

            // void cast - evaluate for side effects, discard
            (_, TypeKind::Void) => {
                self.force_gp(v)?; // materialize and discard
                // push nothing, or a sentinel void value if your vstack needs it
            }

            _ => todo!() // nocheckin
        }

        Ok(())
    }

    fn compile_primary(&mut self) -> CResult<()> {
        match self.current_token.kind {
            TK::Number => {
                // @Cleanup

                let t = self.next();
                let s = self.s(t);
                let is_float_literal = s.contains('.');

                if !is_float_literal {
                    let v = parse_number_int(s);
                    self.vstack.push(CValue::imm(TYPE_INT, v));
                    return Ok(())
                }

                //
                // float literal - store bits in rodata, load via movsd/movss
                //

                let is_float = s.ends_with('f');
                let v = parse_number_float(s);
                let ty = if is_float { TYPE_FLOAT } else { TYPE_DOUBLE };

                let rodata_off = self.rodata.len() as u32;
                if is_float {
                    self.rodata.extend_from_slice(&(v as f32).to_bits().to_le_bytes());
                } else {
                    self.rodata.extend_from_slice(&v.to_bits().to_le_bytes());
                }

                let xmm = self.xmms.alloc(t.span)?;
                let text_off = if is_float {
                    self.code.movss_load_rip(xmm)
                } else {
                    self.code.movsd_load_rip(xmm)
                } as _;

                self.rodata_relocs.push(RodataReloc { text_off, rodata_off });
                self.vstack.push(CValue::xmm(ty, xmm));
            }

            TK::StrLit => {
                let t   = self.next();
                let raw = t.s(&self.pp.src_arena);
                let s   = &raw[1..raw.len()-1];
                let bytes = s.as_bytes();

                // @Refactor: Put this into a separate function

                let mut i = 0;
                while i < bytes.len() {
                    if bytes[i] == b'\\' && i+1 < bytes.len() {
                        i += 1;
                        self.scratch_buffer_for_unescaping_string_literals_to_further_intern_them_into_rodata.push(match bytes[i] {
                            b'n' => b'\n', b't' => b'\t', b'r' => b'\r',
                            b'0' => b'\0', other => other,
                        });
                    } else {
                        self.scratch_buffer_for_unescaping_string_literals_to_further_intern_them_into_rodata.push(bytes[i]);
                    }
                    i += 1;
                }

                // @Incomplete: Implement that in other places too
                //
                // Adjacent string literal concatenation: "hello" "world" -> "helloworld"
                //
                while self.current_token.kind == TK::StrLit {
                    let t2   = self.next();
                    let raw2 = t2.s(&self.pp.src_arena);
                    let s2   = &raw2[1..raw2.len()-1];
                    let bytes2 = s2.as_bytes();
                    let mut i2 = 0;
                    while i2 < bytes2.len() {
                        if bytes2[i2] == b'\\' && i2+1 < bytes2.len() {
                            i2 += 1;
                            self.scratch_buffer_for_unescaping_string_literals_to_further_intern_them_into_rodata.push(match bytes2[i2] {
                                b'n' => b'\n', b't' => b'\t', b'r' => b'\r',
                                b'0' => b'\0', other => other,
                            });
                        } else {
                            self.scratch_buffer_for_unescaping_string_literals_to_further_intern_them_into_rodata.push(bytes2[i2]);
                        }
                        i2 += 1;
                    }
                }

                let bytes = core::mem::take(&mut self.scratch_buffer_for_unescaping_string_literals_to_further_intern_them_into_rodata);
                let rodata_off = self.rodata_intern(&bytes);

                // lea dst, [rip + disp32] - displacement patched by write_elf via R_X86_64_PC32
                let dst = self.regs.alloc(Span::POISONED)?;
                let text_off = self.code.lea_rip(dst) as _;
                self.rodata_relocs.push(RodataReloc { text_off, rodata_off });

                let new_type = self.types.ptr_to(TYPE_CHAR);
                self.vstack.push(CValue::gp(new_type, dst));
            }

            TK::CharLit => {
                let t = self.next();

                let raw = t.s(&self.pp.src_arena);
                let s = &raw[1..raw.len()-1];

                let val = if s.starts_with('\\') {
                    match s.as_bytes().get(1) {
                        Some(b'n')  => b'\n' as i64,
                        Some(b't')  => b'\t' as i64,
                        Some(b'r')  => b'\r' as i64,
                        Some(b'0')  => b'\0' as i64,
                        Some(b'\\') => b'\\' as i64,
                        Some(b'\'') => b'\'' as i64,
                        Some(b'"')  => b'"'  as i64,
                        Some(&b)    => b as i64,
                        None        => 0,
                    }
                } else {
                    s.as_bytes()[0] as i64
                };

                self.vstack.push(CValue::imm(TYPE_INT, val));
            }

            TK::LParen if self.can_hash_start_a_type(self.next_token.hash) => {
                self.next(); // (

                let cast_ty = self.compile_type()?;
                self.expect(TK::RParen, "')'")?;

                if self.current_token.kind == TK::LCurly {
                    // Compound literal
                    let off = self.locals.alloc(0, cast_ty, &self.types);
                    self.compile_initializer(off, cast_ty)?;
                    self.vstack.push(CValue::local(cast_ty, off));
                } else {
                    // Regular cast
                    self.compile_unary()?;
                    let v = self.vstack.pop();
                    self.emit_cast(v, cast_ty)?;
                }
            }

            TK::LParen => {
                self.next();
                self.compile_expr()?;
                self.expect(TK::RParen, "')'")?;
            }

            TK::Ident => {
                let name_tok = self.next();
                let hash     = name_tok.hash;

                if hash == HASH_SIZEOF {
                    let mut has_parens = false;
                    if self.current_token.kind == TK::LParen { self.next(); has_parens = true }

                    let ty = if self.can_hash_start_a_type(self.current_token.hash) {
                        self.compile_type()?
                    } else {
                        self.parse_expr_and_get_its_type()?
                    };

                    if has_parens { self.expect(TK::RParen, "')'")?; }

                    if self.types.get(ty).is_vla() {
                        let r = if let Some(&size_off) = self.vla_sizes.get(&ty) {
                            //
                            // Named local VLA - single load from cached size
                            //

                            let r = self.regs.alloc(Span::POISONED)?;
                            self.code.mov_load(r, Reg::Rbp, size_off, true);
                            r
                        } else {
                            //
                            // Type-form sizeof(int[n][m]) - walk and multiply
                            //

                            self.compute_vla_flat_size(ty, Span::POISONED)?
                        };

                        self.vstack.push(CValue::gp(TYPE_INT, r));
                    } else {
                        let size = self.types.size_of(ty);
                        self.vstack.push(CValue::imm(TYPE_INT, size as _));
                    }
                } else if self.current_token.kind == TK::LParen {
                    if !self.try_compile_builtin(hash)? {
                        self.compile_call(hash, name_tok)?;
                    }
                } else if let Some(lv) = self.locals.find(hash) {
                    if lv.is_indirect {
                        //
                        // Load the pointer, then push as RegInd
                        //

                        let r = self.regs.alloc(name_tok.span)?;
                        self.code.mov_load(r, Reg::Rbp, lv.rbp_off, true);
                        self.vstack.push(CValue::regind(lv.ty, r, 0));
                    } else {
                        self.vstack.push(CValue::local(lv.ty, lv.rbp_off));
                    }
                } else if let Some(gv) = self.globals.find(hash) {
                    //
                    // RIP-relative address of global
                    // push as a RegInd with a data/bss reloc
                    //
                    let dst = self.regs.alloc(name_tok.span)?;
                    let text_off = self.code.lea_rip(dst);
                    self.data_relocs.push(DataReloc {
                        text_off: text_off as u32,
                        data_off: gv.data_off,
                        is_bss:   gv.is_bss,
                    });

                    if !self.dont_decay_types_of_array_globals_to_pointers &&
                        self.types.get_kind(gv.ty) == TypeKind::Array
                    {
                        // Array: push as Reg (already have the address from lea_rip)
                        let ptr_ty = self.decay_array_type(gv.ty);
                        self.vstack.push(CValue::gp(ptr_ty, dst));
                    } else {
                        self.vstack.push(CValue::regind(gv.ty, dst, 0));
                    }
                } else if let Some(&val) = self.enum_consts.get(&hash) {
                    self.vstack.push(CValue::imm(TYPE_INT, val));
                }  else {
                    return Err(CError::Undefined {
                        span: name_tok.span,
                        name: self.s(name_tok).to_owned()
                    });
                }
            }

            other => {
                let t = self.current_token;
                return Err(CError::Expected {
                    span: t.span,
                    expected: "expression",
                    got: format!("{other:?}")
                });
            }
        }

        // @Cold
        //
        // Postfix: subscript, field access, postfix ++/--
        //
        loop {
            match self.current_token.kind {
                TK::PlusPlus | TK::MinusMinus => {
                    let op   = self.current_token.kind;
                    let span = self.current_token.span;
                    self.next();

                    let v = self.vstack.pop();
                    if !v.is_lvalue() { return Err(CError::NotLvalue { span }); }

                    let base = v.reg().as_gp();
                    let ty   = v.ty;

                    //
                    // Load old value
                    //
                    let old = self.regs.alloc(Span::POISONED)?;
                    self.emit_int_load(old, base, v.offset(), ty);

                    //
                    // Promote, increment, store back
                    //
                    let tmp = self.regs.alloc(Span::POISONED)?;
                    self.code.mov_rr(tmp, old);
                    let from_size    = self.types.size_of(ty);
                    let promoted_ty  = self.promoted_type(ty);
                    let is_signed    = self.types.is_signed(ty);
                    self.emit_extend(tmp, from_size, self.types.size_of(promoted_ty), is_signed);
                    match op {
                        TK::PlusPlus  => self.code.add_ri8(tmp, 1),
                        _             => self.code.add_ri8(tmp, -1),
                    }
                    self.emit_int_store(base, v.offset(), tmp, ty); // truncates back naturally
                    self.regs.free(tmp);

                    if v.kind() == VK::RegInd { self.regs.free(base); }

                    // Push old value - postfix yields the value before increment
                    self.vstack.push(CValue::gp(ty, old));
                }

                TK::LSquare => {
                    self.next();
                    self.compile_expr()?;
                    self.expect(TK::RSquare, "']'")?;

                    let idx = self.vstack.pop();
                    let ptr = self.pop_vstack_and_decay_array()?;

                    let (elem_ty, base) = match self.types.get_kind(ptr.ty) {
                        TypeKind::Ptr => {
                            let elem_ty = self.types.deref(ptr.ty);
                            (elem_ty, self.force_gp(ptr)?)
                        }
                        _ => return Err(CError::Expected {
                            span: self.current_token.span,
                            expected: "array or pointer",
                            got: self.types.to_string(ptr.ty)
                        })
                    };

                    let idx_r = self.force_gp(idx)?;
                    if self.types.get(elem_ty).is_vla() {
                        let stride_r = self.compute_vla_flat_size(elem_ty, Span::POISONED)?;
                        self.code.imul_rr(idx_r, stride_r);
                        self.regs.free(stride_r);
                    } else {
                        let elem_sz = self.types.size_of(elem_ty) as i32;
                        self.scale_index(idx_r, elem_sz);
                    }
                    self.regs.free(idx_r);
                    self.code.add_rr(base, idx_r);
                    self.vstack.push(CValue::regind(elem_ty, base, 0));
                }

                TK::Dot | TK::Arrow => {
                    let op   = self.current_token.kind;
                    let span = self.current_token.span;
                    self.next();

                    let field_tok  = self.eat_ident("field name")?;
                    let field_hash = field_tok.hash;

                    let v = self.vstack.pop();

                    let (struct_ty, base, base_off) = match op {
                        TK::Arrow |
                        TK::Dot if self.is_ptr(v.ty) => {
                            if !self.is_ptr(v.ty) {
                                return Err(CError::Expected {
                                    span,
                                    expected: "pointer to struct or union",
                                    got: self.types.to_string(v.ty),
                                });
                            }

                            let struct_ty = self.types.deref(v.ty);
                            let base = self.force_gp(v)?;
                            (struct_ty, base, 0i32)
                        }

                        TK::Dot => {
                            if !matches!(self.get_kind(v.ty), TypeKind::Struct | TypeKind::Union) {
                                return Err(CError::Expected {
                                    span,
                                    expected: "struct or union",
                                    got: self.types.to_string(v.ty),
                                });
                            }

                            let base = v.reg().as_gp();
                            (v.ty, base, v.offset())
                        }

                        _ => unreachable!()
                    };

                    //
                    // @Incomplete: Union handling
                    //

                    let e = self.types.get(struct_ty);
                    let (start, count) = (e.field_start(), e.field_count());
                    let field = self.types.find_field(start, count, field_hash)
                        .ok_or_else(|| CError::UnknownField {
                            span: field_tok.span,
                            name: field_tok.s(&self.src_arena).to_owned(),
                        })?;

                    let field_ty  = field.ty;
                    let field_off = base_off + field.offset as i32;

                    self.vstack.push(CValue::regind(field_ty, base, field_off));
                }

                _ => break
            }
        }

        Ok(())
    }

    /// Returns true if the builtin was handled, false if it should fall through to normal call.
    fn try_compile_builtin(&mut self, hash: u64) -> CResult<bool> {
        match hash {
            HASH_BUILTIN_BSWAP16 | HASH_BUILTIN_BSWAP32 | HASH_BUILTIN_BSWAP64 => {
                self.next(); // (
                self.compile_expr_no_comma()?;
                self.expect(TK::RParen, "')'")?;

                let (r, _) = self.pop_reg()?;
                let ty = match hash {
                    HASH_BUILTIN_BSWAP16 => { self.code.bswap(r, 2); TYPE_SHORT }
                    HASH_BUILTIN_BSWAP32 => { self.code.bswap(r, 4); TYPE_INT   }
                    _                    => { self.code.bswap(r, 8); TYPE_LONG  }
                };
                self.vstack.push(CValue::gp(ty, r));
                Ok(true)
            }

            HASH_BUILTIN_CLZ | HASH_BUILTIN_CLZL | HASH_BUILTIN_CLZLL => {
                self.next(); // (
                self.compile_expr_no_comma()?;
                self.expect(TK::RParen, "')'")?;

                let (r, _) = self.pop_reg()?;
                // lzcnt r, r  (or bsr + xor for fallback)
                self.code.lzcnt(r, r);
                self.vstack.push(CValue::gp(TYPE_INT, r));
                Ok(true)
            }

            HASH_BUILTIN_CTZ | HASH_BUILTIN_CTZL | HASH_BUILTIN_CTZLL => {
                self.next(); // (
                self.compile_expr_no_comma()?;
                self.expect(TK::RParen, "')'")?;

                let (r, _) = self.pop_reg()?;
                self.code.tzcnt(r, r);
                self.vstack.push(CValue::gp(TYPE_INT, r));
                Ok(true)
            }

            HASH_BUILTIN_POPCOUNT | HASH_BUILTIN_POPCOUNTL | HASH_BUILTIN_POPCOUNTLL => {
                self.next(); // (
                self.compile_expr_no_comma()?;
                self.expect(TK::RParen, "')'")?;

                let (r, _) = self.pop_reg()?;
                self.code.popcnt(r, r);
                self.vstack.push(CValue::gp(TYPE_INT, r));
                Ok(true)
            }

            HASH_BUILTIN_EXPECT => {
                self.next(); // (
                self.compile_expr_no_comma()?;
                self.expect(TK::Comma, "','")?;

                // Discard the expected value hint for now... @Incomplete
                self.compile_expr_no_comma()?;
                let hint = self.vstack.pop();

                if matches!(hint.kind(), VK::Reg | VK::RegInd) { self.free_reg(hint.reg()); }
                self.expect(TK::RParen, "')'")?;
                // expr result is already on vstack

                Ok(true)
            }

            HASH_BUILTIN_UNREACHABLE => {
                self.next(); // (
                self.expect(TK::RParen, "')'")?;
                self.code.ud2();  // undefined instruction - trap if reached
                // Push a dummy value since expression context expects something
                self.vstack.push(CValue::imm(TYPE_INT, 0));
                Ok(true)
            }

            HASH_BUILTIN_TRAP => {
                self.next(); // (
                self.expect(TK::RParen, "')'")?;
                self.code.ud2();
                self.vstack.push(CValue::imm(TYPE_INT, 0));
                Ok(true)
            }

            _ => Ok(false)
        }
    }

    #[inline]
    fn emit_memcpy_call(&mut self) -> CResult<()> {
        let call_site = self.code.call_rel32();
        let memcpy_hash = hash_ident("memcpy");
        let idx = if let Some(idx) = self.syms.find(memcpy_hash) {
            idx
        } else {
            self.syms.insert("memcpy", 0, 0, SymFlags::EXTERN, None)
        };

        if self.syms[idx].flags.contains(SymFlags::EXTERN) {
            self.call_relocs.push(CallReloc {
                offset: call_site as u32,
                sym_index: idx as u32,
                addend: -4,
            });
        } else {
            self.code.patch_call(call_site, self.syms[idx].code_off as usize);
        }

        self.regs.clobber_caller_save();
        self.xmms.clobber_caller_save();

        Ok(())
    }

    fn emit_struct_copy(
        &mut self,
        dst_base: Reg, dst_off: i32,
        src_base: Reg, src_off: i32,
        size: i32
    ) -> CResult<()> {
        //
        // @Note: This could use the alignment of the type,
        // which would be a good @CodeOptimization.
        //

        if size <= 64 {
            let tmp = self.regs.alloc(Span::POISONED)?;

            let mut copied = 0i32;
            while copied + 8 <= size {
                self.code.mov_load(tmp, src_base, src_off + copied, true);
                self.code.mov_store(dst_base, dst_off + copied, tmp, true);
                copied += 8;
            }

            if copied + 4 <= size {
                self.code.mov_load(tmp, src_base, src_off + copied, false);
                self.code.mov_store(dst_base, dst_off + copied, tmp, false);
                copied += 4;
            }

            while copied < size {
                self.code.movzx8_load(tmp, src_base, src_off + copied);
                self.code.mov_store8(dst_base, dst_off + copied, tmp);
                copied += 1;
            }

            self.regs.free(tmp);
        } else {
            if src_base == Reg::Rdi {
                // src is gonna get clobbered, alloc a temp register
                let src_tmp = self.regs.alloc(Span::POISONED)?;

                if src_off == 0 { self.code.mov_rr(src_tmp, src_base); }
                else            { self.code.lea(src_tmp, src_base, src_off); }

                if dst_off == 0 { self.code.mov_rr(Reg::Rdi, dst_base); }
                else            { self.code.lea(Reg::Rdi, dst_base, dst_off); }

                self.code.mov_rr(Reg::Rsi, src_tmp);

                // free the temp register
                self.regs.free(src_tmp);
            } else {
                if dst_off == 0 { self.code.mov_rr(Reg::Rdi, dst_base); }
                else            { self.code.lea(Reg::Rdi, dst_base, dst_off); }
                if src_off == 0 { self.code.mov_rr(Reg::Rsi, src_base); }
                else            { self.code.lea(Reg::Rsi, src_base, src_off); }
            }

            self.code.mov_ri64(Reg::Rdx, size as i64);

            self.emit_memcpy_call()?;  // memcpy(dst(rdi), src(rsi), n(rdx))
        }

        Ok(())
    }

    fn spill_vstack_across_call(&mut self) -> CResult<()> {
        for i in 0..self.vstack.len() {
            let v = self.decay_array(self.vstack.vals[i])?;
            match v.kind() {
                VK::Imm | VK::Local => continue, // Already safe
                VK::Reg | VK::RegInd => {}
            }

            let spill_off = self.locals.alloc(HASH_HIDDEN_LOCAL, v.ty, &self.types);

            if matches!(self.get_kind(v.ty), TypeKind::Struct | TypeKind::Union) {
                //
                // Struct in RegInd - base reg will be clobbered
                // Copy struct data to a stack local
                //

                let size = self.types.size_of(v.ty) as i32;

                let (src_base, src_off) = (v.reg().as_gp(), v.offset());
                self.emit_struct_copy(Reg::Rbp, spill_off, src_base, src_off, size)?;
                self.regs.free(src_base);
            } else if self.is_float(v.ty) {
                let xmm = if v.kind() == VK::RegInd {
                    let tmp = self.xmms.alloc(Span::POISONED)?;
                    self.emit_float_load(tmp, v.reg().as_gp(), v.offset(), v.ty);
                    self.regs.free(v.reg().as_gp());
                    tmp
                } else {
                    v.reg().as_xmm()
                };

                self.emit_float_store(Reg::Rbp, spill_off, xmm, v.ty);

                if v.kind() == VK::RegInd { self.xmms.free(xmm); }
                // VK::Reg xmm is freed by clobber_caller_save - don't free here
            } else {
                let gp = if v.kind() == VK::RegInd {
                    let tmp = self.regs.alloc(Span::POISONED)?;
                    self.emit_int_load(tmp, v.reg().as_gp(), v.offset(), v.ty);
                    self.regs.free(v.reg().as_gp());
                    tmp
                } else {
                    v.reg().as_gp()
                };

                self.emit_int_store(Reg::Rbp, spill_off, gp, v.ty);

                if v.kind() == VK::RegInd { self.regs.free(gp); }
                // VK::Reg gp is freed by clobber_caller_save - don't free here
            }

            self.vstack.vals[i] = CValue::local(v.ty, spill_off);
        }

        Ok(())
    }

    fn compile_call(&mut self, callee_hash: u64, name_tok: Token) -> CResult<()> {
        #[derive(Copy, Clone)]
        enum CalleeKind {
            Sym(usize),
            LocalFp(i32),        // rbp_off
            GlobalFp(u32, bool), // data_off, is_bss
        }

        self.next(); // '('

        //
        // Resolve callee - either a named symbol or a local function pointer
        //

        let (func_ty, callee_kind) = if let Some(sym_index) = self.syms.find(callee_hash) {
            //
            // Call to a declared function
            //

            let func_ty = self.syms[sym_index].func_ty;
            (func_ty, CalleeKind::Sym(sym_index))
        } else if let Some(lv) = self.locals.find(callee_hash) {
            //
            // Call to a function pointer inside a local
            //

            match self.types.resolve_func_ptr_type(lv.ty) {
                Some(func_ty) => (func_ty, CalleeKind::LocalFp(lv.rbp_off)),
                None => return Err(CError::Undefined {
                    span: name_tok.span,
                    name: self.s(name_tok).to_owned()
                }),
            }
        } else if let Some(gv) = self.globals.find(callee_hash) {
            //
            // Call to a function pointer inside a global
            //

            match self.types.resolve_func_ptr_type(gv.ty) {
                Some(func_ty) => (func_ty, CalleeKind::GlobalFp(gv.data_off, gv.is_bss)),
                None => return Err(CError::Undefined {
                    span: name_tok.span,
                    name: self.s(name_tok).to_owned()
                }),
            }
        } else {
            return Err(CError::Undefined {
                span: name_tok.span,
                name: self.s(name_tok).to_owned()
            });
        };

        let func_type_entry = self.types.get(func_ty);
        let param_count = func_type_entry.param_count();
        let ret_ty = func_type_entry.ret_ty();
        let is_variadic = func_type_entry.is_variadic();

        //
        // If calling through a function pointer, spill it to a hidden local now
        // so that arg evaluation can't clobber it
        //
        let fp_spill_off = match callee_kind {
            CalleeKind::Sym(_) => None,

            CalleeKind::LocalFp(rbp_off) => {
                let spill = self.locals.alloc(HASH_HIDDEN_LOCAL, TYPE_LONG, &self.types);

                let r = self.regs.alloc(name_tok.span)?;
                self.code.mov_load(r, Reg::Rbp, rbp_off, true);
                self.code.mov_store(Reg::Rbp, spill, r, true);
                self.regs.free(r);

                Some(spill)
            }

            CalleeKind::GlobalFp(data_off, is_bss) => {
                let spill = self.locals.alloc(HASH_HIDDEN_LOCAL, TYPE_LONG, &self.types);

                let r = self.regs.alloc(name_tok.span)?;
                let text_off = self.code.lea_rip(r);  // lea r, [rip + global] then load the pointer value
                self.data_relocs.push(DataReloc { text_off: text_off as u32, data_off, is_bss });
                let r2 = self.regs.alloc(name_tok.span)?;
                self.code.mov_load(r2, r, 0, true);
                self.regs.free(r);

                self.code.mov_store(Reg::Rbp, spill, r2, true);
                self.regs.free(r2);

                Some(spill)
            }
        };

        //
        // Evaluate all args and spill to locals
        // This ensures nested calls don't clobber already-evaluated args
        //
        //                             off   ty     arg span
        let mut arg_spills: SmallVec<[(i32, TypeRef, Span); 8]> = SmallVec::new();

        while self.current_token.kind != TK::RParen && !self.at_eof() {
            //
            // Spill any live registers before evaluating this arg
            //
            // This isn't that good for code quality though...
            // So looking into avoiding some of the spills would be a nice @CodeOptimization
            //
            self.spill_vstack_across_call()?;

            let arg_span = {
                let s = self.current_token.span;
                self.compile_expr_no_comma()?;
                s.merge(self.current_token.span)
            };
            let v = self.pop_vstack_and_decay_array()?;

            //
            // Get declared param type for coercion
            //
            let arg_idx = arg_spills.len();
            let declared_ty = if arg_idx < param_count as usize {
                self.types.get_param(func_ty, arg_idx as _)
            } else {
                v.ty  // variadic, use value type
            };

            let spill_off = self.locals.alloc(HASH_HIDDEN_LOCAL, declared_ty, &self.types);

            self.vstack.push(v);  // emit_store pops from vstack
            self.emit_store(Reg::Rbp, spill_off, declared_ty)?;

            arg_spills.push((spill_off, declared_ty, arg_span));

            if self.current_token.kind == TK::Comma { self.next(); }
        }

        let rparen = self.expect(TK::RParen, "')'")?;
        let call_span = name_tok.span.merge(rparen.span);

        //
        // Check arg counts
        //
        let total_argc = arg_spills.len();
        if is_variadic {
            if total_argc < param_count as usize {
                return Err(CError::ArgumentCountMismatch {
                    span: call_span,
                    expected: param_count as _,
                    name: name_tok.s(&self.src_arena).to_owned()
                });
            }
        } else if total_argc != param_count as usize {
            return Err(CError::ArgumentCountMismatch {
                span: call_span,
                expected: param_count as _,
                name: name_tok.s(&self.src_arena).to_owned()
            });
        }

        //
        // Spill anything still live on the vstack before we load arg registers
        //
        self.spill_vstack_across_call()?;

        //
        // Load spilled args into arg registers
        // No expressions evaluated here so no clobbering possible
        //
        let mut argc     = 0usize;
        let mut xmm_argc = 0usize;
        let mut stack_bytes_pushed = 0i32;

        //
        // Handle struct return
        //

        //                         off is_memory
        let struct_ret_off: Option<(i32, bool)> = if matches!(self.get_kind(ret_ty), TypeKind::Struct | TypeKind::Union) {
            let classes = self.classify_struct(ret_ty);
            if classes[0] == SysVClass::Memory {
                //
                // Sret pointer
                //

                argc = 1;

                let ret_off = self.locals.alloc(HASH_HIDDEN_LOCAL, ret_ty, &self.types);
                self.code.lea(ARG_REGS[0], Reg::Rbp, ret_off);

                Some((ret_off, true))
            } else {
                //
                // Register return
                //

                Some((self.locals.alloc(0, ret_ty, &self.types), false))
            }
        } else {
            None
        };

        for &(spill_off, ty, arg_span) in &arg_spills {
            let kind = self.get_kind(ty);
            if matches!(kind, TypeKind::Struct | TypeKind::Union) {
                //
                // Struct argument
                //

                stack_bytes_pushed += self.pass_struct_arg(
                    spill_off, ty,
                    &mut argc, &mut xmm_argc,
                    call_span, arg_span,
                )?;
            } else if kind.is_float() {
                //
                // Float argument
                //

                if xmm_argc >= XMM_ARG_REGS.len() {
                    return Err(CError::ArgumentCountMismatch {
                        span: call_span,
                        expected: XMM_ARG_REGS.len(),
                        name: name_tok.s(&self.src_arena).to_owned()
                    });
                }

                let dst = XMM_ARG_REGS[xmm_argc];
                if is_variadic && kind == TypeKind::Float {
                    //
                    // Load as float then promote to double (SYSV)
                    //
                    self.code.movss_load(dst, Reg::Rbp, spill_off);
                    self.code.cvtss2sd(dst, dst);
                } else {
                    self.emit_float_load(dst, Reg::Rbp, spill_off, ty);
                }

                xmm_argc += 1;
            } else {
                //
                // Integer argument
                //

                if argc >= ARG_REGS.len() {
                    return Err(CError::ArgumentCountMismatch {
                        span: call_span,
                        expected: ARG_REGS.len(),
                        name: name_tok.s(&self.src_arena).to_owned()
                    });
                }

                self.emit_int_load(ARG_REGS[argc], Reg::Rbp, spill_off, ty);
                argc += 1;
            }
        }

        //
        // al = number of xmm args (SYSV)
        //
        if is_variadic {
            if xmm_argc == 0 {
                self.code.xor_rr(Reg::Rax, Reg::Rax);
            } else {
                self.code.mov_ri64(Reg::Rax, xmm_argc as i64);
            }
        }

        //
        // Emit call instruction
        //
        if let Some(spill) = fp_spill_off {
            //
            // Function pointer call - reload into r11 (not an arg reg, caller-save)
            //
            let r = self.regs.alloc(name_tok.span)?;
            self.code.mov_load(r, Reg::Rbp, spill, true);
            self.code.call_r(r);
            self.regs.free(r);
        } else {
            let CalleeKind::Sym(sym_index) = callee_kind else {
                unreachable!()
            };

            let sym = self.syms[sym_index];
            let call_site = self.code.call_rel32();
            if sym.flags.contains(SymFlags::EXTERN) {
                self.call_relocs.push(CallReloc {
                    offset: call_site as u32,
                    sym_index: sym_index as u32,
                    addend: -4
                });
            } else {
                self.code.patch_call(call_site, sym.code_off as usize);
            }
        }

        self.regs.clobber_caller_save();
        self.xmms.clobber_caller_save();

        if stack_bytes_pushed > 0 {
            self.code.add_ri32(Reg::Rsp, stack_bytes_pushed);
        }

        if let Some((ret_off, is_memory)) = struct_ret_off {
            if !is_memory {
                self.unpack_struct_return_regs(ret_off, ret_ty)?;
            }

            self.vstack.push(CValue::local(ret_ty, ret_off));
        } else if self.is_float(ret_ty) {
            self.xmms.mark(XmmReg::Xmm0);
            self.vstack.push(CValue::xmm(ret_ty, XmmReg::Xmm0));
        } else {
            self.regs.mark(Reg::Rax);
            self.vstack.push(CValue::gp(ret_ty, Reg::Rax));
        }

        Ok(())
    }

    #[inline]
    fn unpack_struct_return_regs(&mut self, ret_off: i32, ret_ty: TypeRef) -> CResult<()> {
        let classes = self.classify_struct(ret_ty);
        let size = self.types.size_of(ret_ty) as i32;

        for (i, &cls) in classes.iter().enumerate() {
            let offset = (i * 8) as i32;
            let chunk  = (size - offset).min(8);

            match cls {
                SysVClass::Integer => {
                    let src = if i == 0 { Reg::Rax } else { Reg::Rdx };

                    if chunk >= 8 {
                        self.code.mov_store(Reg::Rbp, ret_off + offset, src, true);
                    } else if chunk >= 4 {
                        self.code.mov_store(Reg::Rbp, ret_off + offset, src, false);
                    } else {
                        self.code.mov_store8(Reg::Rbp, ret_off + offset, src);
                    }
                }

                SysVClass::Sse => {
                    let xmm = if i == 0 { XmmReg::Xmm0 } else { XmmReg::Xmm1 };

                    self.code.movq_xmm_store(Reg::Rbp, ret_off + offset, xmm);
                }

                SysVClass::Memory => unreachable!(),
            }
        }

        Ok(())
    }

    #[inline]
    fn pack_struct_into_return_regs(&mut self, src_base: Reg, src_off: i32, ret_ty: TypeRef) -> CResult<()> {
        let classes = self.classify_struct(ret_ty);
        let size = self.types.size_of(ret_ty) as i32;

        for (i, &cls) in classes.iter().enumerate() {
            let offset = (i * 8) as i32;
            let chunk  = (size - offset).min(8);

            match cls {
                SysVClass::Integer => {
                    let dst = if i == 0 { Reg::Rax } else { Reg::Rdx };

                    self.code.xor_rr(dst, dst);

                    if chunk >= 8 {
                        self.code.mov_load(dst, src_base, src_off + offset, true);
                    } else if chunk >= 4 {
                        self.code.mov_load(dst, src_base, src_off + offset, false);
                    } else {
                        let tmp = self.regs.alloc(Span::POISONED)?;
                        let mut b = 0i32;
                        while b < chunk {
                            self.code.movzx8_load(tmp, src_base, src_off + offset + b);
                            self.code.shl_ri(dst, 8);
                            self.code.or_rr(dst, tmp);
                            b += 1;
                        }
                        self.regs.free(tmp);
                    }
                }

                SysVClass::Sse => {
                    let xmm = if i == 0 { XmmReg::Xmm0 } else { XmmReg::Xmm1 };

                    if chunk >= 8 {
                        self.code.movq_xmm_load(xmm, src_base, src_off + offset);
                    } else if chunk >= 4 {
                        self.code.movss_load(xmm, src_base, src_off + offset);
                    } else {
                        self.code.movss_load(xmm, src_base, src_off + offset);
                    }
                }

                SysVClass::Memory => unreachable!(),
            }
        }

        Ok(())
    }

    #[inline]
    fn unpack_struct_from_arg_regs(
        &mut self,
        off: i32,
        ty: TypeRef,

        argc: &mut usize,
        xmm_argc: &mut usize,

        // For errors
        call_span: Span,
        arg_span: Span,
    ) -> CResult<()> {
        let classes = self.classify_struct(ty);
        let size = self.types.size_of(ty) as i32;
        for (i, &cls) in classes.iter().enumerate() {
            let offset = (i * 8) as i32;
            let chunk  = (size - offset).min(8);
            match cls {
                SysVClass::Integer => {
                    if *argc >= ARG_REGS.len() {
                        return Err(CError::ArgumentCountMismatch {
                            span: call_span,
                            expected: ARG_REGS.len(),
                            name: arg_span.s(&self.src_arena).to_owned()
                        });
                    }

                    let src = ARG_REGS[*argc];
                    *argc += 1;

                    if chunk >= 8 {
                        self.code.mov_store(Reg::Rbp, off + offset, src, true);
                    } else if chunk >= 4 {
                        self.code.mov_store(Reg::Rbp, off + offset, src, false);
                    } else {
                        self.code.mov_store8(Reg::Rbp, off + offset, src);
                    }
                }

                SysVClass::Sse => {
                    if *xmm_argc >= XMM_ARG_REGS.len() {
                        return Err(CError::ArgumentCountMismatch {
                            span: call_span,
                            expected: XMM_ARG_REGS.len(),
                            name: arg_span.s(&self.src_arena).to_owned()
                        });
                    }

                    let xmm = XMM_ARG_REGS[*xmm_argc];
                    *xmm_argc += 1;
                    self.code.movq_xmm_store(Reg::Rbp, off + offset, xmm);
                }

                SysVClass::Memory => unreachable!(),
            }
        }

        Ok(())
    }

    fn pass_struct_arg(
        &mut self,

        spill_off: i32,
        ty: TypeRef,

        argc: &mut usize,
        xmm_argc: &mut usize,

        // For errors
        call_span: Span,
        arg_span: Span,
    ) -> CResult<i32> {
        let classes = self.classify_struct(ty);

        if classes[0] == SysVClass::Memory {
            //
            // Copy the struct to the stack space
            //

            let size = self.types.size_of(ty) as i32;
            let aligned = (size + 15) & !15;
            self.code.sub_rsp(aligned);
            self.emit_struct_copy(Reg::Rsp, 0, Reg::Rbp, spill_off, size)?;
            return Ok(aligned);
        }

        let size = self.types.size_of(ty);

        for (i, &cls) in classes.iter().enumerate() {
            let offset = (i * 8) as i32;
            let chunk  = (size as i32 - offset).min(8);
            match cls {
                SysVClass::Integer => {
                    if *argc >= ARG_REGS.len() {
                        return Err(CError::ArgumentCountMismatch {
                            span: call_span,
                            expected: ARG_REGS.len(),
                            name: arg_span.s(&self.src_arena).to_owned()
                        });
                    }

                    let r = ARG_REGS[*argc];
                    *argc += 1;

                    self.code.xor_rr(r, r);
                    if chunk >= 8 {
                        self.code.mov_load(r, Reg::Rbp, spill_off + offset, true);
                    } else if chunk >= 4 {
                        self.code.mov_load(r, Reg::Rbp, spill_off + offset, false);
                    } else {
                        // Just in case... Load 4 bytes, upper bits zeroed by xor above
                        self.code.mov_load(r, Reg::Rbp, spill_off + offset, false);
                    }
                }

                SysVClass::Sse => {
                    if *xmm_argc >= XMM_ARG_REGS.len() {
                        return Err(CError::ArgumentCountMismatch {
                            span: call_span,
                            expected: XMM_ARG_REGS.len(),
                            name: arg_span.s(&self.src_arena).to_owned()
                        });
                    }

                    let xmm = XMM_ARG_REGS[*xmm_argc];
                    self.code.movq_xmm_load(xmm, Reg::Rbp, spill_off + offset);
                    *xmm_argc += 1;
                }

                SysVClass::Memory => unreachable!(),
            }
        }

        Ok(0)
    }

    //
	// Type Coercion Infrastructure
    //

	/// Pure-type UAC: returns the common TypeRef without emitting any code.
	/// Used for constant-folding fast paths where we just need the result type.
	#[inline]
	fn get_usual_arithmetic_conversion_type(&self, lty: TypeRef, rty: TypeRef) -> TypeRef {
	    let lk = self.get_kind(lty);
	    let rk = self.get_kind(rty);

        //
	    // Float dominance
        //
	    if lk == TypeKind::Double || rk == TypeKind::Double { return TYPE_DOUBLE }
	    if lk == TypeKind::Float  || rk == TypeKind::Float  { return TYPE_FLOAT  }

        //
	    // Integer promotion brings both to at least int
        //
	    let lty = if matches!(lk, TypeKind::Bool | TypeKind::Char | TypeKind::Short) { TYPE_INT } else { lty };
	    let rty = if matches!(rk, TypeKind::Bool | TypeKind::Char | TypeKind::Short) { TYPE_INT } else { rty };
	    if lty == rty { return lty }

	    let l_unsigned = self.types.is_unsigned(lty);
	    let r_unsigned = self.types.is_unsigned(rty);

	    let lk = self.get_kind(lty);
	    let rk = self.get_kind(rty);

	    let l_rank = Self::int_rank(lk);
	    let r_rank = Self::int_rank(rk);

	    if l_unsigned == r_unsigned {
	        if l_rank >= r_rank { lty } else { rty }
	    } else {
	        let (u_rank, s_rank, u_ty, s_ty) = if l_unsigned {
	            (l_rank, r_rank, lty, rty)
	        } else {
	            (r_rank, l_rank, rty, lty)
	        };

	        if u_rank >= s_rank { u_ty } else { s_ty }
	    }
	}

	/// Returns the integer-promoted type for a single type (char/short/bool -> int).
	#[inline]
	fn promoted_type(&self, ty: TypeRef) -> TypeRef {
	    match self.get_kind(ty) {
	        TypeKind::Bool | TypeKind::Char | TypeKind::Short => TYPE_INT,
	        _ => ty,
	    }
	}

	/// Integer conversion rank per C11 $6.3.1.1.
	/// Returns None for non-integer types (float, ptr, struct, …).
	#[inline]
	const fn int_rank(kind: TypeKind) -> u8 {
	    match kind {
	        TypeKind::Bool  => 0,
	        TypeKind::Char  => 1,
	        TypeKind::Short => 2,
	        TypeKind::Int   => 3,
	        TypeKind::Long  => 4,
	        TypeKind::LLong => 5,
	        _               => 3, // fallback, shouldn't be called on non-int
	    }
	}

	/// Integer promotion: char/short/bool -> int (C11 $6.3.1.1 p2)
	/// No-op for types already at int rank or above.
	#[inline]
	fn integer_promote(&mut self, v: CValue) -> CResult<CValue> {
	    match self.get_kind(v.ty) {
	        TypeKind::Bool | TypeKind::Char | TypeKind::Short => {
	            // Imm path: just retag, full i64 bits are already correct
	            if v.kind() == VK::Imm {
	                return Ok(CValue::imm(TYPE_INT, v.imm));
	            }

	            let r = self.force_gp(v)?;
	            let from_size = self.types.size_of(v.ty);
	            let is_signed = self.types.is_signed(v.ty);
	            self.emit_extend(r, from_size, 4, is_signed);
	            Ok(CValue::gp(TYPE_INT, r))
	        }

	        _ => Ok(v),
	    }
	}

	/// Usual arithmetic conversions (C11 $6.3.1.8).
	/// Returns (coerced_lhs, coerced_rhs, common_type).
	///
	/// Float dominance:
	///   double > float (long double omitted - no hw regs yet)
	///
	/// Integer:
	///   - Apply integer promotions to both sides
	///   - Same type -> done
	///   - Same signedness -> higher rank wins
	///   - Unsigned rank >= signed rank -> unsigned wins
	///   - Otherwise signed wins (it can represent all unsigned values)
	#[inline]
	fn do_usual_arithmetic_conversion(&mut self, lv: CValue, rv: CValue) -> CResult<(CValue, CValue, TypeRef)> {
	    let lk = self.get_kind(lv.ty);
	    let rk = self.get_kind(rv.ty);

        //
	    // Float dominance
        //
	    let l_float = matches!(lk, TypeKind::Float | TypeKind::Double);
	    let r_float = matches!(rk, TypeKind::Float | TypeKind::Double);
	    if l_float || r_float {
	        let common = if lk == TypeKind::Double || rk == TypeKind::Double {
	            TYPE_DOUBLE
	        } else {
	            TYPE_FLOAT
	        };
	        let l = self.coerce_to_xmm(lv, common)?;
	        let r = self.coerce_to_xmm(rv, common)?;
	        return Ok((CValue::xmm(common, l), CValue::xmm(common, r), common));
	    }

        //
	    // Integer promotions
        //
	    let lv = self.integer_promote(lv)?;
	    let rv = self.integer_promote(rv)?;

	    if lv.ty == rv.ty {
	        return Ok((lv, rv, lv.ty));
	    }

	    let l_unsigned = self.types.is_unsigned(lv.ty);
	    let r_unsigned = self.types.is_unsigned(rv.ty);

	    let lk = self.get_kind(lv.ty);
	    let rk = self.get_kind(rv.ty);

	    let l_rank = Self::int_rank(lk);
	    let r_rank = Self::int_rank(rk);

	    let common_ty = if l_unsigned == r_unsigned {
	        if l_rank >= r_rank { lv.ty } else { rv.ty }
	    } else {
	        let (u_rank, s_rank, u_ty, s_ty) = if l_unsigned {
	            (l_rank, r_rank, lv.ty, rv.ty)
	        } else {
	            (r_rank, l_rank, rv.ty, lv.ty)
	        };
	        if u_rank >= s_rank { u_ty } else { s_ty }
	    };

	    let lv = self.coerce_int(lv, common_ty)?;
	    let rv = self.coerce_int(rv, common_ty)?;
	    Ok((lv, rv, common_ty))
	}

	/// Coerce an integer CValue to `target` integer type.
	/// Extends (sign/zero) upward, truncation is implicit on x86-64.
	#[inline]
	fn coerce_int(&mut self, v: CValue, target: TypeRef) -> CResult<CValue> {
	    if v.ty == target { return Ok(v) }
	    if v.kind() == VK::Imm { return Ok(CValue::imm(target, v.imm)) }

	    let r = self.force_gp(v)?;
	    let from_size = self.types.size_of(v.ty);
	    let to_size   = self.types.size_of(target);
	    if to_size > from_size {
	        let is_signed = self.types.is_signed(v.ty);
	        self.emit_extend(r, from_size, to_size, is_signed);
	    }

	    Ok(CValue::gp(target, r))
	}

	/// Implicit coercion for assignment / argument passing (C11 $6.5.16.1).
	/// Clang-default: all four categories silently allowed.
	/// TODO: hook warning flags here when they exist.
	#[inline]
	fn coerce_for_assign(&mut self, v: CValue, target: TypeRef) -> CResult<CValue> {
	    if v.ty == target { return Ok(v); }

	    let from_kind   = self.get_kind(v.ty);
	    let target_kind = self.get_kind(target);

	    match (from_kind, target_kind) {
	        // int <-> int
	        (TypeKind::Bool  | TypeKind::Char  | TypeKind::Short |
	         TypeKind::Int   | TypeKind::Long  | TypeKind::LLong,
	         TypeKind::Bool  | TypeKind::Char  | TypeKind::Short |
	         TypeKind::Int   | TypeKind::Long  | TypeKind::LLong) => {
	            // TODO(flags): warn on narrowing if -Wconversion
	            self.coerce_int(v, target)
	        }

	        // int -> float
	        (TypeKind::Bool  | TypeKind::Char  | TypeKind::Short |
	         TypeKind::Int   | TypeKind::Long  | TypeKind::LLong,
	         TypeKind::Float | TypeKind::Double) => {
	            // TODO(flags): warn on precision loss if -Wfloat-conversion
	            if v.kind() == VK::Imm {
                    //
	                // Constant fold: store bits in a fresh Imm with fimm set
                    //
	                let fval = v.imm as f64;
	                return Ok(CValue::fimm(target, fval));
	            }

	            let r = self.force_gp(v)?;
	            let xmm = self.xmms.alloc(Span::POISONED)?;
	            if target_kind == TypeKind::Float {
	                self.code.cvtsi2ss(xmm, r);
	            } else {
	                self.code.cvtsi2sd(xmm, r);
	            }
	            self.regs.free(r);
	            Ok(CValue::xmm(target, xmm))
	        }

	        // float -> int
	        (TypeKind::Float | TypeKind::Double,
	         TypeKind::Bool  | TypeKind::Char  | TypeKind::Short |
	         TypeKind::Int   | TypeKind::Long  | TypeKind::LLong) => {
	            // TODO(flags): warn always (-Wfloat-conversion)
	            let xmm = self.force_xmm(v)?;
	            let r = self.regs.alloc(Span::POISONED)?;
	            if from_kind == TypeKind::Float {
	                self.code.cvttss2si(r, xmm);
	            } else {
	                self.code.cvttsd2si(r, xmm);
	            }
	            self.xmms.free(xmm);
	            // If target is narrower than 64-bit, truncate
	            let r = {
	                let tmp = CValue::gp(TYPE_LONG, r);
	                self.coerce_int(tmp, target)?
	            };
	            Ok(r)
	        }

	        // float <-> float
	        (TypeKind::Float, TypeKind::Double) => {
	            if v.kind() == VK::Imm {
	                return Ok(CValue::fimm(target, v.get_fimm()));
	            }
	            let xmm = self.force_xmm(v)?;
	            self.code.cvtss2sd(xmm, xmm);
	            Ok(CValue::xmm(target, xmm))
	        }
	        (TypeKind::Double, TypeKind::Float) => {
	            // TODO(flags): warn on precision loss if -Wfloat-conversion
	            if v.kind() == VK::Imm {
	                return Ok(CValue::fimm(target, v.get_fimm() as f32 as f64));
	            }
	            let xmm = self.force_xmm(v)?;
	            self.code.cvtsd2ss(xmm, xmm);
	            Ok(CValue::xmm(target, xmm))
	        }

	        // ptr <-> ptr (void* and others)
	        (TypeKind::Ptr, TypeKind::Ptr) => {
	            // TODO(flags): warn on non-void* incompatible pointer if -Wincompatible-pointer-types
	            if v.kind() == VK::Imm { return Ok(CValue::imm(target, v.imm)); }
	            let r = self.force_gp(v)?;
	            Ok(CValue::gp(target, r))
	        }

	        // int <-> ptr
	        (TypeKind::Int | TypeKind::Long | TypeKind::LLong, TypeKind::Ptr) |
	        (TypeKind::Ptr, TypeKind::Int | TypeKind::Long | TypeKind::LLong) => {
	            // TODO(flags): warn if -Wint-conversion
	            if v.kind() == VK::Imm { return Ok(CValue::imm(target, v.imm)); }
	            let r = self.force_gp(v)?;
	            Ok(CValue::gp(target, r))
	        }

            //
	        // signed <-> unsigned (same width)
	        // Already handled by coerce_int above (same size = no extend needed).
	        // Explicit arm for clarity; no-op on x86-64.
            //

	        //  everything else: structs, arrays, void - no-op
	        _ => Ok(v),
	    }
	}
}

pub fn write_elf(c: &Compiler) -> Vec<u8> {
    let nsyms = c.syms.len();
    let ngvars = c.globals.vars.len();

    //
    // strtab
    //

    let mut strtab = Vec::with_capacity(c.syms.name_buf.len() + nsyms); // +1 null per sym
    strtab.push(0u8);

    let mut sym_name_index  = Vec::with_capacity(nsyms);
    let mut gvar_name_index = Vec::with_capacity(ngvars);

    for i in 0..c.syms.len() {
        sym_name_index.push(strtab.len() as u32);
        strtab.extend_from_slice(c.syms.s(i).as_bytes());
        strtab.push(0);
    }
    for gv in c.globals.vars.iter() {
        gvar_name_index.push(strtab.len() as u32);
        strtab.extend_from_slice(gv.s(&c.globals.name_buf).as_bytes());
        strtab.push(0);
    }

    //
    // shstrtab
    //

    let mut shstrtab = vec![0u8];
    let mut sname = |s: &str| -> u32 {
        let off = shstrtab.len() as u32;
        shstrtab.extend_from_slice(s.as_bytes()); shstrtab.push(0); off
    };
    let sh_text     = sname(".text");
    let sh_rodata   = sname(".rodata");
    let sh_data     = sname(".data");
    let sh_bss      = sname(".bss");
    let sh_rela     = sname(".rela.text");
    let sh_symtab   = sname(".symtab");
    let sh_strtab   = sname(".strtab");
    let sh_shstrtab = sname(".shstrtab");

    //
    // symtab
    //
    // Layout:
    //   [0]  null
    //   [1]  STT_SECTION .rodata
    //   [2]  STT_SECTION .data
    //   [3]  STT_SECTION .bss
    //   [4+] defined functions (static, global)
    //   [..] global variables  (STT_OBJECT)
    //   [..] extern functions  (undefined)
    //
    const SHN_UNDEF:   u16 = 0;
    const SHN_TEXT:    u16 = 1;
    const SHN_RODATA:  u16 = 2;
    const SHN_DATA:    u16 = 3;
    const SHN_BSS:     u16 = 4;
    const STB_LOCAL:   u8  = 0;
    const STB_GLOBAL:  u8  = 1;
    const STT_FUNC:    u8  = 2;
    const STT_OBJECT:  u8  = 1;
    const STT_NOTYPE:  u8  = 0;
    const STT_SECTION: u8  = 3;

    let mut symtab = Vec::with_capacity((nsyms + 2) * 24);
    let push_sym = |symtab: &mut Vec<u8>, name: u32, info: u8, shndx: u16, value: u64, size: u64| {
        symtab.extend_from_slice(&name.to_le_bytes());
        symtab.push(info); symtab.push(0);
        symtab.extend_from_slice(&shndx.to_le_bytes());
        symtab.extend_from_slice(&value.to_le_bytes());
        symtab.extend_from_slice(&size.to_le_bytes());
    };

    push_sym(&mut symtab, 0, 0, SHN_UNDEF, 0, 0);                           // [0] null
    push_sym(&mut symtab, 0, (STB_LOCAL<<4)|STT_SECTION, SHN_RODATA, 0, 0); // [1] .rodata
    push_sym(&mut symtab, 0, (STB_LOCAL<<4)|STT_SECTION, SHN_DATA,   0, 0); // [2] .data
    push_sym(&mut symtab, 0, (STB_LOCAL<<4)|STT_SECTION, SHN_BSS,    0, 0); // [3] .bss

    const RODATA_SYM: u64 = 1;
    const DATA_SYM:   u64 = 2;
    const BSS_SYM:    u64 = 3;

    //
    // First go static functions and globals
    //
    for (i, sym) in c.syms.iter().enumerate() {
        if sym.flags.contains(SymFlags::EXTERN) { continue; }
        if !sym.flags.contains(SymFlags::STATIC)  { continue; }

        push_sym(&mut symtab, sym_name_index[i], (STB_LOCAL<<4)|STT_FUNC, SHN_TEXT, sym.code_off as u64, sym.code_len as u64);
    }
    for (i, gv) in c.globals.vars.iter().enumerate() {
        if !gv.is_static { continue; }

        let shndx = if gv.is_bss { SHN_BSS } else { SHN_DATA };
        push_sym(&mut symtab, gvar_name_index[i], (STB_LOCAL<<4)|STT_OBJECT, shndx, gv.data_off as u64, c.size_of(gv.ty) as u64);
    }

    //
    // Now go global functions and globals
    //

    let first_global_sym = symtab.len() / 24; // sh_info = this

    for (i, sym) in c.syms.iter().enumerate() {
        if sym.flags.contains(SymFlags::EXTERN) { continue; }
        if sym.flags.contains(SymFlags::STATIC)  { continue; }

        push_sym(&mut symtab, sym_name_index[i], (STB_GLOBAL<<4)|STT_FUNC, SHN_TEXT, sym.code_off as u64, sym.code_len as u64);
    }
    for (i, gv) in c.globals.vars.iter().enumerate() {
        if gv.is_static { continue; }

        let shndx = if gv.is_bss { SHN_BSS } else { SHN_DATA };
        push_sym(&mut symtab, gvar_name_index[i], (STB_GLOBAL<<4)|STT_OBJECT, shndx, gv.data_off as u64, c.size_of(gv.ty) as u64);
    }

    let mut elf_sym_index = vec![0u32; nsyms];
    for (i, sym) in c.syms.iter().enumerate() {
        if !sym.flags.contains(SymFlags::EXTERN) { continue; }
        elf_sym_index[i] = (symtab.len() / 24) as u32;
        push_sym(&mut symtab, sym_name_index[i], (STB_GLOBAL<<4)|STT_NOTYPE, SHN_UNDEF, 0, 0);
    }

    //
    // rela.text
    //
    const R_PLT32: u64 = 4;
    const R_PC32:  u64 = 2;
    let mut rela = Vec::new();
    let push_rela = |rela: &mut Vec<u8>, offset: u64, sym: u64, rtype: u64, addend: i64| {
        rela.extend_from_slice(&offset.to_le_bytes());
        rela.extend_from_slice(&((sym<<32)|rtype).to_le_bytes());
        rela.extend_from_slice(&addend.to_le_bytes());
    };

    for r in &c.call_relocs {
        push_rela(&mut rela, r.offset as u64, elf_sym_index[r.sym_index as usize] as u64, R_PLT32, r.addend);
    }
    for r in &c.rodata_relocs {
        push_rela(&mut rela, r.text_off as u64, RODATA_SYM, R_PC32, r.rodata_off as i64 - 4);
    }
    for r in &c.data_relocs {
        let sym = if r.is_bss { BSS_SYM } else { DATA_SYM };
        push_rela(&mut rela, r.text_off as u64, sym, R_PC32, r.data_off as i64 - 4);
    }

    //
    // Layout
    //
    const EHSZ: usize = 64;
    const SHSZ: usize = 64;
    const NSEC: usize = 9;

    let text_off    = EHSZ;
    let text_sz     = c.code.bytes.len();
    let rodata_off  = align(text_off   + text_sz,        16); let rodata_sz  = c.rodata.len();
    let data_off    = align(rodata_off + rodata_sz,       16); let data_sz    = c.data.len();
    let rela_off    = align(data_off   + data_sz,          8); let rela_sz    = rela.len();
    let sym_off     = align(rela_off   + rela_sz,          8); let sym_sz     = symtab.len();
    let str_off     = align(sym_off    + sym_sz,           8); let str_sz     = strtab.len();
    let shstr_off   = align(str_off    + str_sz,           8); let shstr_sz   = shstrtab.len();
    let shdrs_off   = align(shstr_off  + shstr_sz,         8);

    let mut out = vec![0u8; shdrs_off + NSEC * SHSZ];

    //
    // ELF header
    //
    out[0..4].copy_from_slice(b"\x7fELF");
    out[4]=2; out[5]=1; out[6]=1; out[7]=0;
    out[16..18].copy_from_slice(&1u16.to_le_bytes());   // ET_REL
    out[18..20].copy_from_slice(&62u16.to_le_bytes());  // EM_X86_64
    out[20..24].copy_from_slice(&1u32.to_le_bytes());
    out[40..48].copy_from_slice(&(shdrs_off as u64).to_le_bytes());
    out[52..54].copy_from_slice(&(EHSZ as u16).to_le_bytes());
    out[58..60].copy_from_slice(&(SHSZ as u16).to_le_bytes());
    out[60..62].copy_from_slice(&(NSEC as u16).to_le_bytes());
    out[62..64].copy_from_slice(&8u16.to_le_bytes()); // e_shstrndx = .shstrtab

    //
    // Section data
    //
    out[text_off  ..text_off  +text_sz  ].copy_from_slice(&c.code.bytes);
    out[rodata_off..rodata_off+rodata_sz].copy_from_slice(&c.rodata);
    out[data_off  ..data_off  +data_sz  ].copy_from_slice(&c.data);
    // .bss has no file content - zero sized in file
    out[rela_off  ..rela_off  +rela_sz  ].copy_from_slice(&rela);
    out[sym_off   ..sym_off   +sym_sz   ].copy_from_slice(&symtab);
    out[str_off   ..str_off   +str_sz   ].copy_from_slice(&strtab);
    out[shstr_off ..shstr_off +shstr_sz ].copy_from_slice(&shstrtab);

    //
    // Section headers
    //
    let section_header = |
        out: &mut [u8], i: usize, name: u32, ty: u32, flags: u64,
        off: u64, sz: u64, link: u32, info: u32, align: u64, esz: u64
    | {
        let b = shdrs_off + i * SHSZ;
        out[b+ 0..b+ 4].copy_from_slice(&name.to_le_bytes());
        out[b+ 4..b+ 8].copy_from_slice(&ty.to_le_bytes());
        out[b+ 8..b+16].copy_from_slice(&flags.to_le_bytes());
        out[b+16..b+24].copy_from_slice(&0u64.to_le_bytes());
        out[b+24..b+32].copy_from_slice(&off.to_le_bytes());
        out[b+32..b+40].copy_from_slice(&sz.to_le_bytes());
        out[b+40..b+44].copy_from_slice(&link.to_le_bytes());
        out[b+44..b+48].copy_from_slice(&info.to_le_bytes());
        out[b+48..b+56].copy_from_slice(&align.to_le_bytes());
        out[b+56..b+64].copy_from_slice(&esz.to_le_bytes());
    };

    const NULL:     u32 = 0;
    const PROGBITS: u32 = 1;
    const SYMTAB:   u32 = 2;
    const STRTAB:   u32 = 3;
    const RELA:     u32 = 4;
    const NOBITS:   u32 = 8;  // .bss
    const ALLOC:    u64 = 0x2;
    const WRITE:    u64 = 0x1;
    const EXEC:     u64 = 0x4;

    //
    // Section indices:
    // 0=null 1=.text 2=.rodata 3=.data 4=.bss 5=.rela.text 6=.symtab 7=.strtab 8=.shstrtab
    //

    section_header(&mut out, 0, 0,           NULL,     0,           0,                 0,                0, 0,                       0,  0);
    section_header(&mut out, 1, sh_text,     PROGBITS, ALLOC|EXEC,  text_off   as u64, text_sz   as u64, 0, 0,                       16, 0);
    section_header(&mut out, 2, sh_rodata,   PROGBITS, ALLOC,       rodata_off as u64, rodata_sz as u64, 0, 0,                       16, 0);
    section_header(&mut out, 3, sh_data,     PROGBITS, ALLOC|WRITE, data_off   as u64, data_sz   as u64, 0, 0,                       16, 0);
    section_header(&mut out, 4, sh_bss,      NOBITS,   ALLOC|WRITE, data_off   as u64, c.bss_size as u64,0, 0,                       16, 0);
    section_header(&mut out, 5, sh_rela,     RELA,     0,           rela_off   as u64, rela_sz   as u64, 6, 1,                       8,  24); // link=symtab(6), info=.text(1)
    section_header(&mut out, 6, sh_symtab,   SYMTAB,   0,           sym_off    as u64, sym_sz    as u64, 7, first_global_sym as u32, 8,  24); // link=strtab(7)
    section_header(&mut out, 7, sh_strtab,   STRTAB,   0,           str_off    as u64, str_sz    as u64, 0, 0,                       1,  0);
    section_header(&mut out, 8, sh_shstrtab, STRTAB,   0,           shstr_off  as u64, shstr_sz  as u64, 0, 0,                       1,  0);

    out
}

fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() < 2 {
        eprintln!("usage: ccrush <file.c> [-o out.o]"); std::process::exit(1);
    }

    if args.contains(&"-debug-tokens".into()) {
        debug_tokens(Path::new(&args[1]));
        return;
    }

    let out_path = args.iter()
        .position(|s| s == "-o")
        .and_then(|i| args.get(i+1)).map(|s| s.as_str()).unwrap_or("out.o");

    let mut pp = match PP::new(Path::new(&args[1]), &args[1..]) {
        Ok(pp) => pp,
        Err(e) => { e.emit(&SrcArena::new()); std::process::exit(1); }
    };

    for arg in &args {
        //
        // -DFOO or -DFOO=BAR args
        //
        if let Some(def) = arg.strip_prefix("-D") {
            let (name, val) = if let Some(eq) = def.find('=') {
                (&def[..eq], &def[eq+1..])
            } else {
                (def, "1")
            };
            pp.define_simple(name, val);
        }
    }

    let mut c = Compiler::new(pp);
    c.compile();

    // #[cfg(debug_assertions)]
    // for (i, e) in c.types.entries.iter() {
    //     eprintln!("  [{i:?}] {:?} quals={:?} ref_={:?} extra={}", e.kind, e.quals, e.ref_, e.extra);
    // }

    if args.contains(&"-run".into()) {
        run_main(c);
        return;
    }

    let elf = write_elf(&c);
    std::fs::write(out_path, &elf).unwrap_or_else(|e| {
        eprintln!("write {out_path}: {e}");
        std::process::exit(1);
    });

    eprintln!("wrote {out_path} ({} bytes text, {} total)", c.code.bytes.len(), elf.len());
}

fn run_main(mut c: Compiler) {
    use std::ffi::CString;

    //
    // Layout: [code][rodata][data][bss_zeros][trampolines]
    //
    let rodata_base = c.code.bytes.len();
    c.code.bytes.extend_from_slice(&c.rodata);

    let data_base = c.code.bytes.len();
    c.code.bytes.extend_from_slice(&c.data);

    let bss_base = c.code.bytes.len();
    c.code.bytes.extend(std::iter::repeat(0u8).take(c.bss_size));

    //
    // Patch rodata relocs
    //
    for r in &c.rodata_relocs {
        let target    = rodata_base + r.rodata_off as usize;
        let patch_pos = r.text_off as usize;
        let rel       = (target as i64) - (patch_pos as i64 + 4);
        c.code.patch_i32(patch_pos, rel as i32);
    }

    //
    // Patch data/bss relocs
    //
    for r in &c.data_relocs {
        let base      = if r.is_bss { bss_base } else { data_base };
        let target    = base + r.data_off as usize;
        let patch_pos = r.text_off as usize;
        let rel       = (target as i64) - (patch_pos as i64 + 4);
        c.code.patch_i32(patch_pos, rel as i32);
    }

    //
    // Trampolines
    //
    let mut sym_to_trampoline = IntMap::default();
    for r in &c.call_relocs {
        let sym_index = r.sym_index as usize;
        if sym_to_trampoline.contains_key(&sym_index) { continue; }

        let sym_name   = c.syms.s(sym_index);
        let sym_name_c = CString::new(sym_name).unwrap();
        let sym_addr   = unsafe { libc::dlsym(libc::RTLD_DEFAULT, sym_name_c.as_ptr()) } as i64;
        if sym_addr == 0 {
            eprintln!("undefined symbol: {sym_name}");
            std::process::exit(1);
        }

        let trampoline_off = c.code.bytes.len();
        sym_to_trampoline.insert(sym_index, trampoline_off);

        c.code.mov_ri64(Reg::R11, sym_addr);
        c.code.jmp_r(Reg::R11);
    }

    //
    // Prepare argc/argv/envp
    //
    let args = std::env::args().map(|s| CString::new(s).unwrap()).collect::<Vec<_>>();
    let argv = args.iter().map(|s| s.as_ptr() as *const u8).chain([std::ptr::null()]).collect::<Vec<_>>();
    let argc = args.len() as i32;
    let env_vars = std::env::vars().map(|(k,v)| CString::new(format!("{k}={v}")).unwrap()).collect::<Vec<_>>();
    let envp = env_vars.iter().map(|s| s.as_ptr() as *const u8).chain([std::ptr::null()]).collect::<Vec<_>>();

    //
    // Find main
    //
    let main_hash = hash_ident("main");
    let Some(main_idx) = c.syms.find(main_hash) else {
        eprintln!("no main function"); std::process::exit(1);
    };
    let main_sym = &c.syms[main_idx];
    if main_sym.flags.contains(SymFlags::EXTERN) {
        eprintln!("main not defined"); std::process::exit(1);
    }
    let main_off = main_sym.code_off as usize;

    //
    // Mmap and patch extern relocs
    //
    let mut mmap = MmapMut::map_anon(c.code.bytes.len()).unwrap();
    mmap.copy_from_slice(&c.code.bytes);

    let base = mmap.as_ptr() as i64;
    for r in &c.call_relocs {
        let trampoline_off = sym_to_trampoline[&(r.sym_index as usize)];
        let patch_pos      = r.offset as usize;
        let rel = (base + trampoline_off as i64 - (base + patch_pos as i64 + 4)) as i32;
        unsafe {
            std::ptr::write_unaligned(mmap.as_mut_ptr().add(patch_pos) as *mut i32, rel);
        }
    }

    // Make entire mapping RWX - code needs exec (.data needs write)
    unsafe {
        libc::mprotect(
            mmap.as_mut_ptr() as *mut libc::c_void,
            mmap.len(),
            libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
        );
    }

    let f: extern "C" fn(i32, *const *const u8, *const *const u8) -> i32 = unsafe {
        std::mem::transmute(base as usize + main_off)
    };

    let argv_ptr = argv.as_ptr();
    let envp_ptr = envp.as_ptr();
    let result = f(argc, argv_ptr, envp_ptr);
    std::process::exit(result);
}
