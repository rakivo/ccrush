use std::fs::File;
use std::path::{Path, PathBuf};
use std::ops::{Deref, DerefMut};

use thiserror::Error;
use smallvec::SmallVec;
use nohash_hasher::IntMap;
use memmap2::{Mmap, MmapMut, MmapOptions};

#[inline]
const fn fnv1a(b: &[u8]) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    let mut i = 0;
    while i < b.len() {
        let x = b[i];
        h = (h ^ x as u64).wrapping_mul(0x100000001b3);
        i += 1;
    }
    h
}

#[inline]
const fn fnv1a_str(s: &str) -> u64 {
    fnv1a(s.as_bytes())
}

#[inline]
const fn align(x: usize, a: usize) -> usize {
    (x + a - 1) & !(a - 1)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct FileId(pub u16);

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
    pub files: Vec<FileInfo>,
}

impl SrcArena {
    #[inline]
    pub fn new() -> Self {
        Self { files: Vec::with_capacity(64) }
    }

    // for real files - mmap'd, no copy
    #[inline]
    pub fn add_path(&mut self, path: &Path) -> std::io::Result<FileId> {
        let file    = std::fs::File::open(path)?;
        let mapping = unsafe { MmapOptions::new().map(&file)? };
        let id      = FileId(self.files.len() as u16);
        self.files.push(FileInfo {
            path: path.to_string_lossy().into(),
            data: FileData::Mapped(mapping),
        });
        Ok(id)
    }

    // for tests / PP::from_bytes - owned vec, no mmap
    #[inline]
    pub fn add_bytes(&mut self, path: &Path, src: impl Into<Box<[u8]>>) -> FileId {
        let src = src.into();
        let id   = FileId(self.files.len() as u16);
        self.files.push(FileInfo {
            path: path.to_string_lossy().into(),
            data: FileData::Owned(src),
        });

        id
    }

    #[inline]
    pub fn slice(&self, fid: FileId) -> &[u8] {
        self.files[fid.0 as usize].data.slice()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub file: FileId,
    pub start: u32,
    pub len: u16
}

impl Span {
    pub const POISONED: Span = unsafe { core::mem::zeroed() };

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

pub fn emit_diag(msg: &str, span: Span, arena: &SrcArena) {
    const R: &str = "\x1b[1;31m"; const C: &str = "\x1b[36m";
    const B: &str = "\x1b[1m";   const X: &str = "\x1b[0m";

    eprintln!("{R}error{X}{B}: {msg}{X}");
    if span == Span::POISONED { return; }

    let src    = arena.slice(span.file);
    let path   = &arena.files[span.file.0 as usize].path;
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
    eprintln!("{pad} {C}|{X} {}{R}{}{X}", " ".repeat(hl_s), "^".repeat(hl_l));
    eprintln!("{pad} {C}|{X}");
}

#[derive(Debug, Error)]
pub enum PPError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

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

    #[error("undefined symbol '{name}'")]
    Undefined   { span: Span, name: String },

    #[error("lvalue required")]
    NotLvalue   { span: Span },

    #[error("argument count mismatch for '{name}' (expected {expected})")]
    ArgumentCountMismatch { span: Span, name: String, expected: usize },

    #[error("register spill - not yet implemented")]
    RegSpill    { span: Span },
}

impl CError {
    #[inline]
    pub fn span(&self) -> Span {
        match self {
            CError::Expected    { span, .. } => *span,
            CError::UnknownType { span, .. } => *span,
            CError::Undefined   { span, .. } => *span,
            CError::NotLvalue   { span, .. } => *span,
            CError::ArgumentCountMismatch { span, .. } => *span,
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
    Eof, Ident, Number, StrLit,
    LParen, RParen, LCurly, RCurly, Comma, SemiColon, TripleDot,
    Plus, Minus, Star, Slash, Eq,
    PlusEq, MinusEq, StarEq, SlashEq,
    EqEq, NotEq,
    Less, Greater, LessEq, GreaterEq, BinAnd, BinOr, Not,

    // PP-internal - never escapes cooked stream
    Hash, Newline,

    // Inside macro bodies only
    Param(u8),
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
const HASH_RETURN:  u64 = fnv1a_str("return");
const HASH_INT:     u64 = fnv1a_str("int");
const HASH_LONG:    u64 = fnv1a_str("long");
const HASH_CHAR:    u64 = fnv1a_str("char");
const HASH_VOID:    u64 = fnv1a_str("void");
const HASH_FLOAT:   u64 = fnv1a_str("float");
const HASH_DOUBLE:  u64 = fnv1a_str("double");
const HASH_ONCE:    u64 = fnv1a_str("once");
const HASH_EXTERN:  u64 = fnv1a_str("extern");
const HASH_DEFINE:  u64 = fnv1a_str("define");
const HASH_INCLUDE: u64 = fnv1a_str("include");
const HASH_PRAGMA:  u64 = fnv1a_str("pragma");
const HASH_UNDEF:   u64 = fnv1a_str("undef");
const HASH_IFNDEF:  u64 = fnv1a_str("ifndef");
const HASH_IFDEF:   u64 = fnv1a_str("ifdef");
const HASH_IF:      u64 = fnv1a_str("if");
const HASH_FOR:     u64 = fnv1a_str("for");
const HASH_WHILE:   u64 = fnv1a_str("while");
const HASH_ELSE:    u64 = fnv1a_str("else");
const HASH_ELIF:    u64 = fnv1a_str("elif");
const HASH_ENDIF:   u64 = fnv1a_str("endif");

const HASH_TYPES: &[u64] = &[
    HASH_INT, HASH_LONG, HASH_CHAR, HASH_VOID, HASH_FLOAT, HASH_DOUBLE
];

fn lex(src: &[u8], pos: &mut usize, fid: FileId) -> Token {
    // Skip spaces, tabs, carriage returns.  Written as an iterator so LLVM
    // can auto-vectorise the scan rather than emitting a byte-at-a-time loop.
    let skip = src[*pos..].iter().position(|&b| !matches!(b, b' '|b'\t'|b'\r')).unwrap_or(src.len() - *pos);
    *pos += skip;
    if *pos >= src.len() {
        return Token { kind: TK::Eof, span: Span { file: fid, start: *pos as u32, len: 0 }, hash: 0 };
    }
    let start = *pos;
    let ch = src[*pos]; *pos += 1;

    macro_rules! tok  { ($k:expr) => {
        Token { kind: $k, span: Span { file: fid, start: start as u32, len: (*pos-start) as u16 }, hash: 0 } }
    }
    macro_rules! tok2 {
        ($c:expr,$y:expr,$n:expr) => {{
            if *pos < src.len() && src[*pos]==$c { *pos+=1; tok!($y) } else { tok!($n) }
        }}
    }

    match ch {
        b'\n' => tok!(TK::Newline),
        b'('  => tok!(TK::LParen),  b')' => tok!(TK::RParen),
        b'{'  => tok!(TK::LCurly),  b'}' => tok!(TK::RCurly),
        b','  => tok!(TK::Comma),   b';' => tok!(TK::SemiColon),
        b'+' => tok2!(b'=', TK::PlusEq,  TK::Plus),
        b'-' => tok2!(b'=', TK::MinusEq, TK::Minus),
        b'*' => tok2!(b'=', TK::StarEq,  TK::Star),
        b'#'  => tok!(TK::Hash),
        b'&'  => tok!(TK::BinAnd),  b'|' => tok!(TK::BinOr),
        b'!'  => tok2!(b'=', TK::NotEq,     TK::Not),
        b'<'  => tok2!(b'=', TK::LessEq,    TK::Less),
        b'>'  => tok2!(b'=', TK::GreaterEq, TK::Greater),
        b'='  => tok2!(b'=', TK::EqEq,      TK::Eq),

        b'.'  => {
            if *pos+1 < src.len() && src[*pos]==b'.' && src[*pos+1]==b'.' {
                *pos += 2; tok!(TK::TripleDot)
            }
            else { lex(src, pos, fid) } // Skip stray dot
        }

        b'/' => {
            if *pos < src.len() && src[*pos] == b'/' {
                while *pos < src.len() && src[*pos] != b'\n' { *pos += 1; }
                Token {
                    kind: TK::Newline,
                    span: Span { file: fid, start: start as u32, len: (*pos-start) as u16 },
                    hash: 0
                }
            } else if *pos < src.len() && src[*pos] == b'=' {
                *pos += 1; tok!(TK::SlashEq)
            } else {
                tok!(TK::Slash)
            }
        }

        b'"' => {
            while *pos < src.len() && src[*pos] != b'"' && src[*pos] != b'\n' {
                if src[*pos] == b'\\' { *pos += 1; }
                *pos += 1;
            }
            if *pos < src.len() { *pos += 1; }
            tok!(TK::StrLit)
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

const MAX_PARAMS: usize = 8;
const MAX_DEPTH:  usize = 32;

#[derive(Clone, Copy)]
struct MacroDef {
    name_hash:    u64,
    def_span:     Span,
    body_start:   u32,
    body_len:     u32,
    param_count:  u8,
    param_hashes: [u64; MAX_PARAMS],
}

impl MacroDef {
    const ZERO: Self = Self {
        name_hash: 0, def_span: Span::POISONED, body_start: 0, body_len: 0,
        param_count: 0, param_hashes: [0; MAX_PARAMS],
    };
}

struct MacroTable {
    defs:     SmallVec<[MacroDef; 64]>,
    tok_pool: SmallVec<[Token; 512]>,

    index:    IntMap<u64, u32>,

    scratch:  Vec<Token>,  // Reused across calls, never shrinks

    // Expanded argument pool - reused across expand_func_macro calls.
    // arg_pool holds the cooked tokens for all args of the current call;
    // arg_ends[i] is the exclusive end index in arg_pool for argument i.
    arg_pool: Vec<Token>,
    arg_ends: SmallVec<[u32; MAX_PARAMS]>,
}

impl MacroTable {
    #[inline]
    fn new() -> Self {
        Self {
            defs: SmallVec::new(),
            index: IntMap::with_capacity_and_hasher(64, Default::default()),
            tok_pool: SmallVec::new(),
            scratch: Vec::with_capacity(256),
            arg_pool: Vec::with_capacity(256),
            arg_ends: SmallVec::new(),
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
    fn define(&mut self, mut def: MacroDef, body: &[Token]) {
        let start = self.tok_pool.len() as u32;

        def.body_start = start;
        def.body_len   = body.len() as u32;

        self.tok_pool.extend_from_slice(body);

        if let Some(&i) = self.index.get(&def.name_hash) {
            self.defs[i as usize] = def;
            return;
        }

        let i = self.defs.len() as u32;
        self.index.insert(def.name_hash, i);
        self.defs.push(def);
    }

    #[inline]
    fn undef(&mut self, hash: u64) {
        if let Some(&i) = self.index.get(&hash) {
            self.index.remove(&hash);
            self.defs[i as usize].name_hash = 0;
        }
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
    fid: FileId,
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
            pool:   Vec::with_capacity(512),
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

pub struct PP {
    src_arena:      SrcArena,

    pub current_token: Token,
    pub next_token:    Token,

    file_stack:     Vec<FileFrame>,
    exp:            Expansions,
    at_bol:         bool, // At beginning of line - gate for # directives

    pragma_once_paths: Vec<PathBuf>,

    include_dirs:   Vec<PathBuf>,

    macros:         MacroTable,
}

impl PP {
    #[inline]
    pub fn from_path(path: &Path) -> PPResult<Self> {
        let mut arena = SrcArena::new();
        let fid = arena.add_path(path)?;
        Ok(Self::init(arena, fid))
    }

    #[inline]
    pub fn from_bytes(src: Vec<u8>) -> Self {
        let mut arena = SrcArena::new();
        let fid = arena.add_bytes(Path::new("<input>"), src);
        Self::init(arena, fid)
    }

    #[inline]
    fn init(arena: SrcArena, fid: FileId) -> Self {
        let mut pp = Self {
            src_arena: arena,
            file_stack:         vec![FileFrame { fid, pos: 0 }],
            exp:                Expansions::new(),
            macros:             MacroTable::new(),
            include_dirs:       vec![PathBuf::from("/usr/include").into(), PathBuf::from("/usr/local/include").into()],
            pragma_once_paths: Vec::new(),
            at_bol:             true,
            current_token:      Token::EOF,
            next_token:         Token::EOF,
        };
        pp.current_token  = pp.cook();
        pp.next_token = pp.cook();
        pp
    }

    #[inline]
    pub fn add_include_dir(&mut self, p: impl Into<PathBuf>) {
        self.include_dirs.push(p.into());
    }

    // Slide window, return consumed token.
    #[inline]
    pub fn next(&mut self) -> Token {
        let t   = self.current_token;
        self.current_token  = self.next_token;
        self.next_token = self.cook();
        t
    }

    #[inline]
    pub fn s(&self, t: Token) -> &str { t.s(&self.src_arena) }

    #[inline]
    fn raw(&mut self) -> Token {
        loop {
            if let Some(frame) = self.exp.frames.last_mut() {
                let (_, end, cursor) = frame;
                if *cursor < *end {
                    let t = self.exp.pool[*cursor as usize];
                    *cursor += 1;
                    return t;
                }
                self.exp.pop();
            } else {
                break;
            }
        }

        while let Some(ff) = self.file_stack.last_mut() {
            let data = self.src_arena.files[ff.fid.0 as usize].data.slice();
            if ff.pos < data.len() {
                return lex(data, &mut ff.pos, ff.fid);
            }

            self.file_stack.pop();
        }

        Token::EOF
    }

    #[inline]
    fn cook(&mut self) -> Token {
        loop {
            let t = self.raw();
            match t.kind {
                TK::Newline => self.at_bol = true,

                TK::Hash if self.at_bol => {
                    if let Err(e) = self.directive() {
                        e.emit(&self.src_arena);
                        std::process::exit(1);
                    }
                }

                TK::Ident => {
                    self.at_bol = false;

                    let hash = fnv1a_str(t.s(&self.src_arena));
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
                _       => { self.at_bol = false; return t; }
            }
        }
    }

    #[inline]
    fn directive(&mut self) -> PPResult<()> {
        let mut name = self.raw();
        if name.kind != TK::Ident {
            self.skip_line();
            return Ok(());
        }

        name.hash = fnv1a_str(name.s(&self.src_arena));

        match name.hash {
            HASH_DEFINE  => self.pp_define(),
            HASH_INCLUDE => self.pp_include(name.span),
            HASH_PRAGMA  => { self.pp_pragma(); Ok(()) }
            HASH_UNDEF   => { self.pp_undef();  Ok(()) }

            HASH_IFNDEF | HASH_IFDEF | HASH_IF | HASH_ELSE | HASH_ELIF | HASH_ENDIF => {
                // @Incomplete
                self.skip_line(); Ok(())
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

    #[inline]
    fn skip_line(&mut self) {
        loop {
            let t = self.raw();
            if matches!(t.kind, TK::Newline|TK::Eof) { break; }
        }
    }

    fn pp_define(&mut self) -> PPResult<()> {
        let name_tok = self.raw();
        if name_tok.kind != TK::Ident { self.skip_line(); return Ok(()); }

        let name_hash = fnv1a_str(name_tok.s(&self.src_arena));
        let next      = self.raw();

        // Function macro: '(' must be immediately adjacent - no whitespace
        let is_func = next.kind == TK::LParen
            && next.span.file  == name_tok.span.file
            && next.span.start == name_tok.span.start + name_tok.span.len as u32;

        let mut def  = MacroDef {
            name_hash, def_span: name_tok.span, ..MacroDef::ZERO
        };
        let mut body = Vec::new();

        #[inline]
        fn try_param_subst(t: Token, def: &MacroDef, arena: &SrcArena) -> Token {
            if t.kind != TK::Ident || def.param_count == 0 { return t; }

            let h = fnv1a_str(t.s(arena));
            for i in 0..def.param_count as usize {
                if def.param_hashes[i] == h {
                    return Token { kind: TK::Param(i as u8), span: t.span, hash: 0 };
                }
            }

            t
        }

        if is_func {
            loop {
                let t = self.raw();
                match t.kind {
                    TK::RParen            => break,
                    TK::Comma             => {}
                    TK::Ident             => {
                        let ph = fnv1a_str(t.s(&self.src_arena));
                        def.param_hashes[def.param_count as usize] = ph;
                        def.param_count += 1;
                    }
                    TK::Newline | TK::Eof  => { self.skip_line(); return Ok(()); }
                    _ => {}
                }
            }

            loop {
                let t = self.raw();
                if matches!(t.kind, TK::Newline|TK::Eof) { break; }
                body.push(try_param_subst(t, &def, &self.src_arena));
            }
        } else if !matches!(next.kind, TK::Newline|TK::Eof) {
            body.push(try_param_subst(next, &def, &self.src_arena));
            loop {
                let t = self.raw();
                if matches!(t.kind, TK::Newline|TK::Eof) { break; }
                body.push(try_param_subst(t, &def, &self.src_arena));
            }
        }

        self.macros.define(def, &body);

        Ok(())
    }

    #[inline]
    fn pp_undef(&mut self) {
        let t = self.raw();
        if t.kind == TK::Ident {
            self.macros.undef(fnv1a_str(t.s(&self.src_arena)));
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
                (raw[1..raw.len()-1].to_owned(), false)
            }

            TK::Less => {
                let mut s = String::new();
                loop {
                    let t = self.raw();
                    if matches!(t.kind, TK::Greater | TK::Newline | TK::Eof) { break; }
                    s.push_str(t.s(&self.src_arena));
                }
                (s, true)
            }

            _ => { self.skip_line(); return Ok(()); }
        };
        self.skip_line();

        let cur_dir = {
            let fid  = self.file_stack.last().map(|f| f.fid).unwrap_or(FileId(0));
            let parent_dir_opt = Path::new(self.src_arena.files[fid.0 as usize].path.as_ref()).parent();
            parent_dir_opt.unwrap_or(Path::new(".")).to_owned()
        };

        let resolved = if is_sys {
            self.find_sys(&path_str)
        } else {
            let l = cur_dir.join(&path_str);
            if l.exists() { Some(l) } else { self.find_sys(&path_str) }
        };

        let Some(resolved) = resolved else {
            return Err(PPError::NotFound { span: first.span, path: path_str });
        };

        if self.pragma_once_paths.contains(&resolved) { return Ok(()); }

        let file = File::open(&resolved)?;
        let mmap = unsafe { MmapOptions::new().map(&file)? };

        let id = FileId(self.src_arena.files.len() as u16);
        self.src_arena.files.push(FileInfo {
            path: resolved.to_string_lossy().into(),
            data: FileData::Mapped(mmap),
        });
        self.file_stack.push(FileFrame { fid: id, pos: 0 });

        Ok(())
    }

    #[inline]
    fn find_sys(&self, name: &str) -> Option<PathBuf> {
        self.include_dirs.iter().map(|d| d.join(name)).find(|p| p.exists())
    }

    #[inline]
    fn pp_pragma(&mut self) {
        let t = self.raw();
        if t.kind == TK::Ident && fnv1a_str(t.s(&self.src_arena)) == HASH_ONCE {
            if let Some(ff) = self.file_stack.last() {
                let path = Path::new(self.src_arena.files[ff.fid.0 as usize].path.as_ref());
                if let Ok(canonical) = path.canonicalize() {
                    if !self.pragma_once_paths.contains(&canonical) {
                        self.pragma_once_paths.push(canonical);
                    }
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
                    if arg_index >= def.param_count as usize {
                        self.macros.scratch.truncate(scratch_base as usize);
                        return Err(PPError::ArgumentCountMismatch {
                            span: t.span,
                            expected: def.param_count as _,
                            name: def.def_span.s(&self.src_arena).to_owned(),
                        });
                    }

                    arg_start = self.macros.scratch.len() as u32;
                }

                _ => self.macros.scratch.push(t),
            }
        }

        if arg_ranges.len() < def.param_count as usize {
            self.macros.scratch.truncate(scratch_base as usize);
            return Err(PPError::ArgumentCountMismatch {
                span: call_span,
                expected: def.param_count as _,
                name: def.def_span.s(&self.src_arena).to_owned(),
            });
        }

        //
        // Expand each arg from scratch into macros.arg_pool.
        // arg_pool and arg_ends are reset here; expand_slice_into appends to arg_pool.
        //

        let arg_pool_base = self.macros.arg_pool.len() as u32;
        self.macros.arg_ends.clear();

        for &(start, len) in &arg_ranges {
            let s = start as usize;
            let e = s + len as usize;
            // expand_slice_into cooks the scratch slice into macros.arg_pool
            self.expand_slice_into(s, e);
            self.macros.arg_ends.push(self.macros.arg_pool.len() as u32);
        }

        self.macros.scratch.truncate(scratch_base as usize);

        //
        // Substitute body directly into exp.pool - read body coords first to
        // release the borrow on self.macros before touching self.exp.
        //

        let d = &self.macros.defs[index];
        let body_start = d.body_start as usize;
        let body_len   = d.body_len   as usize;
        let frame_start = self.exp.pool.len() as u32;
        let mut frame_len = 0u32;
        for i in body_start..body_start + body_len {
            let t = self.macros.tok_pool[i];
            match t.kind {
                TK::Param(pi) => {
                    let pi = pi as usize;

                    let arg_start = if pi == 0 { arg_pool_base as usize }
                                    else       { self.macros.arg_ends[pi - 1] as usize };
                    let arg_end   = self.macros.arg_ends[pi] as usize;

                    self.exp.pool.extend_from_slice(&self.macros.arg_pool[arg_start..arg_end]);
                    frame_len += (arg_end - arg_start) as u32;
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

    // Cook a slice of raw tokens (indices into macros.scratch) and append the
    // result directly into macros.arg_pool.  No heap allocation.
    #[inline]
    fn expand_slice_into(&mut self, scratch_start: usize, scratch_end: usize) {
        if scratch_start == scratch_end { return; }

        // Copy the arg tokens + EOF sentinel into exp.pool as a temporary frame.
        // We use exp.push so the cook() loop drains it normally.
        let tmp_start = self.exp.pool.len() as u32;
        self.exp.pool.extend_from_slice(&self.macros.scratch[scratch_start..scratch_end]);
        self.exp.pool.push(Token::EOF);
        let tmp_end = self.exp.pool.len() as u32;
        self.exp.frames.push((tmp_start, tmp_end, tmp_start));

        loop {
            let t = self.cook();
            if t.kind == TK::Eof { break; }
            self.macros.arg_pool.push(t);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CType {
    Void,
    Int,
    Long,
    Char,
    Float,
    Double,
    Ptr(u8) // Depth
}

impl CType {
    #[inline]
    pub const fn size(self) -> u8 {
        match self {
            CType::Void => 0,
            CType::Char => 1,
            CType::Int | CType::Float => 4,
            CType::Long | CType::Double | CType::Ptr(_) => 8
        }
    }

    #[inline]
    pub const fn is64(self)  -> bool { self.size() == 8 }
    #[inline]
    pub const fn is_ptr(self)-> bool { matches!(self, CType::Ptr(_)) }
    #[inline]
    pub fn is_float(self) -> bool { matches!(self, CType::Float | CType::Double) }
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

// --- CValue - value stack entry ----------------------------------------------
//   Imm    - compile-time constant
//   Reg    - value in register
//   Local  - [rbp + offset]
//   RegInd - [reg + offset] (register indirection: deref, array, struct field)

#[derive(Clone, Copy, Debug)]
pub struct CValue {
    pub imm:    i64,
    pub fimm:   f64,
    pub ty:     CType,
    pub reg:    ValReg,
    pub kind:   VK,
    pub offset: i32,
}

impl CValue {
    #[inline]
    pub fn imm(ty: CType, v: i64)            -> Self {
        Self { kind: VK::Imm,    ty, reg: ValReg::Gp(Reg::Rax), offset: 0,   imm: v, fimm: 0.0 }
    }

    #[inline]
    pub fn fimm(ty: CType, v: f64)           -> Self {
        Self { kind: VK::Imm,    ty, reg: ValReg::Gp(Reg::Rax), offset: 0,   imm: 0, fimm: v   }
    }

    #[inline]
    pub fn gp(ty: CType, r: Reg)             -> Self {
        Self { kind: VK::Reg,    ty, reg: ValReg::Gp(r),        offset: 0,   imm: 0, fimm: 0.0 }
    }

    #[inline]
    pub fn xmm(ty: CType, r: XmmReg)         -> Self {
        Self { kind: VK::Reg,    ty, reg: ValReg::Xmm(r),       offset: 0,   imm: 0, fimm: 0.0 }
    }

    #[inline]
    pub fn local(ty: CType, off: i32)        -> Self {
        Self { kind: VK::Local,  ty, reg: ValReg::Gp(Reg::Rbp), offset: off, imm: 0, fimm: 0.0 }
    }

    #[inline]
    pub fn regind(ty: CType, r: Reg, o: i32) -> Self {
        Self { kind: VK::RegInd, ty, reg: ValReg::Gp(r),        offset: o,   imm: 0, fimm: 0.0 }
    }

    #[inline]
    pub fn is_lvalue(self) -> bool {
        matches!(self.kind, VK::Local | VK::RegInd)
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
    #[inline]
    pub fn mov_store(&mut self, base: Reg, off: i32, src: Reg, is64: bool) {
        self.rex(is64, src,  base); self.emit_byte(0x89); self.modrm_mem(src,  base, off);
    }
    #[inline]
    pub fn lea      (&mut self, dst: Reg, base: Reg, off: i32)             {
        self.rex_w(dst, base);      self.emit_byte(0x8D); self.modrm_mem(dst,  base, off);
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
        self.rex_w(dst, Reg::Rax);
        self.emit_byte(0x8D);
        self.emit_byte(0x05 | (dst.enc() << 3));
        let patch = self.pos();
        self.emit_i32(0);
        patch
    }

    #[inline]
    pub fn xor_rr   (&mut self, dst: Reg, src: Reg) {
        self.rex_w(src, dst); self.emit_byte(0x33); self.modrm_rr(src, dst);
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
    pub fn imul_rr  (&mut self, dst: Reg, src: Reg) {
        self.rex_w(dst, src); self.bytes.extend_from_slice(&[0x0F, 0xAF]); self.modrm_rr(dst, src);
    }
    #[inline]
    pub fn cqo      (&mut self) { self.bytes.extend_from_slice(&[0x48, 0x99]); }

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

    // Scalar double arithmetic
    #[inline]
    pub fn addsd(&mut self, dst: XmmReg, src: XmmReg) {
        self.bytes.extend_from_slice(&[0xF2, 0x0F, 0x58, 0xC0 | (dst as u8) << 3 | src as u8]);
    }
    #[inline]
    pub fn subsd(&mut self, dst: XmmReg, src: XmmReg) {
        self.bytes.extend_from_slice(&[0xF2, 0x0F, 0x5C, 0xC0 | (dst as u8) << 3 | src as u8]);
    }
    #[inline]
    pub fn mulsd(&mut self, dst: XmmReg, src: XmmReg) {
        self.bytes.extend_from_slice(&[0xF2, 0x0F, 0x59, 0xC0 | (dst as u8) << 3 | src as u8]);
    }
    #[inline]
    pub fn divsd(&mut self, dst: XmmReg, src: XmmReg) {
        self.bytes.extend_from_slice(&[0xF2, 0x0F, 0x5E, 0xC0 | (dst as u8) << 3 | src as u8]);
    }

    // Scalar float arithmetic
    #[inline]
    pub fn addss(&mut self, dst: XmmReg, src: XmmReg) {
        self.bytes.extend_from_slice(&[0xF3, 0x0F, 0x58, 0xC0 | (dst as u8) << 3 | src as u8]);
    }
    #[inline]
    pub fn subss(&mut self, dst: XmmReg, src: XmmReg) {
        self.bytes.extend_from_slice(&[0xF3, 0x0F, 0x5C, 0xC0 | (dst as u8) << 3 | src as u8]);
    }
    #[inline]
    pub fn mulss(&mut self, dst: XmmReg, src: XmmReg) {
        self.bytes.extend_from_slice(&[0xF3, 0x0F, 0x59, 0xC0 | (dst as u8) << 3 | src as u8]);
    }
    #[inline]
    pub fn divss(&mut self, dst: XmmReg, src: XmmReg) {
        self.bytes.extend_from_slice(&[0xF3, 0x0F, 0x5E, 0xC0 | (dst as u8) << 3 | src as u8]);
    }

    #[inline]
    pub fn ucomiss(&mut self, lhs: XmmReg, rhs: XmmReg) {
        self.bytes.extend_from_slice(&[0x0F, 0x2E, 0xC0 | (lhs as u8) << 3 | rhs as u8]);
    }

    #[inline]
    pub fn ucomisd(&mut self, lhs: XmmReg, rhs: XmmReg) {
        self.bytes.extend_from_slice(&[0x66, 0x0F, 0x2E, 0xC0 | (lhs as u8) << 3 | rhs as u8]);
    }

    #[inline]
    pub fn movss_load(&mut self, dst: XmmReg, base: Reg, off: i32) {
        self.bytes.extend_from_slice(&[0xF3, 0x0F, 0x10]);
        self.modrm_mem_impl(dst as u8, base, off);
    }

    #[inline]
    pub fn movss_store(&mut self, base: Reg, off: i32, src: XmmReg) {
        self.bytes.extend_from_slice(&[0xF3, 0x0F, 0x11]);
        self.modrm_mem_impl(src as u8, base, off);
    }

    #[inline]
    pub fn movsd_load(&mut self, dst: XmmReg, base: Reg, off: i32) {
        self.bytes.extend_from_slice(&[0xF2, 0x0F, 0x10]);
        self.modrm_mem_impl(dst as u8, base, off);
    }

    #[inline]
    pub fn movsd_store(&mut self, base: Reg, off: i32, src: XmmReg) {
        self.bytes.extend_from_slice(&[0xF2, 0x0F, 0x11]);
        self.modrm_mem_impl(src as u8, base, off);
    }

    // movsd [rip+disp32], xmm  - store double
    #[inline]
    pub fn movsd_store_rip(&mut self, src: XmmReg) -> usize {
        self.bytes.extend_from_slice(&[0xF2, 0x0F, 0x11]);
        self.bytes.push(0x05 | (src as u8) << 3);
        let patch = self.pos();
        self.emit_i32(0);
        patch
    }

    // movss xmm, [rip+disp32]  - load float from memory
    #[inline]
    pub fn movss_load_rip(&mut self, dst: XmmReg) -> usize {
        self.bytes.extend_from_slice(&[0xF3, 0x0F, 0x10]);
        self.bytes.push(0x05 | (dst as u8) << 3);
        let patch = self.pos(); self.emit_i32(0); patch
    }

    // movsd xmm, [rip+disp32]  - load double from memory
    #[inline]
    pub fn movsd_load_rip(&mut self, dst: XmmReg) -> usize {
        self.bytes.extend_from_slice(&[0xF2, 0x0F, 0x10]);
        self.bytes.push(0x05 | (dst as u8) << 3);
        let patch = self.pos(); self.emit_i32(0); patch
    }

    #[inline]
    pub fn xorpd_rip(&mut self, dst: XmmReg) -> usize {
        self.bytes.extend_from_slice(&[0x66, 0x0F, 0x57]);
        self.bytes.push(0x05 | (dst as u8) << 3);
        let patch = self.pos(); self.emit_i32(0); patch
    }

    #[inline]
    pub fn xorps_rip(&mut self, dst: XmmReg) -> usize {
        self.bytes.extend_from_slice(&[0x0F, 0x57]);
        self.bytes.push(0x05 | (dst as u8) << 3);
        let patch = self.pos(); self.emit_i32(0); patch
    }

    #[inline]
    pub fn movss_rr(&mut self, dst: XmmReg, src: XmmReg) {
        if dst == src { return; }
        self.bytes.extend_from_slice(&[0xF3, 0x0F, 0x10, 0xC0 | (dst as u8) << 3 | src as u8]);
    }

    // movsd xmm, xmm  - reg to reg move
    #[inline]
    pub fn movsd_rr(&mut self, dst: XmmReg, src: XmmReg) {
        if dst == src { return; }
        self.bytes.extend_from_slice(&[0xF2, 0x0F, 0x10, 0xC0 | (dst as u8) << 3 | src as u8]);
    }

    #[inline]
    pub fn cvtsi2sd(&mut self, dst: XmmReg, src: Reg) {
        let rex = 0x48 | (src.ext() as u8);
        self.bytes.extend_from_slice(&[0xF2, rex, 0x0F, 0x2A]);
        self.bytes.push(0xC0 | (dst as u8) << 3 | src.enc());
    }

    // cvtss2sd xmm_dst, xmm_src  - float -> double promotion
    #[inline]
    pub fn cvtss2sd(&mut self, dst: XmmReg, src: XmmReg) {
        self.bytes.extend_from_slice(&[0xF3, 0x0F, 0x5A, 0xC0 | (dst as u8) << 3 | src as u8]);
    }

    #[inline]
    pub fn cvtsd2ss(&mut self, dst: XmmReg, src: XmmReg) {
        self.bytes.extend_from_slice(&[0xF2, 0x0F, 0x5A, 0xC0 | (dst as u8) << 3 | src as u8]);
    }
}

const VALUE_STACK_CAP: usize = 64;

pub struct ValueStack { vals: [CValue; VALUE_STACK_CAP], top: usize }

impl ValueStack {
    pub fn new() -> Self { Self { vals: [CValue::imm(CType::Int, 0); VALUE_STACK_CAP], top: 0 } }
    pub fn push(&mut self, v: CValue) { self.vals[self.top] = v; self.top += 1; }
    #[track_caller]
    pub fn pop (&mut self) -> CValue  { self.top -= 1; self.vals[self.top] }
    pub fn peek(&self)     -> CValue  { self.vals[self.top - 1] }
    pub fn len (&self)     -> usize   { self.top }
}

const MAX_LOCALS: usize = 128;

#[derive(Clone, Copy)]
pub struct Local {
    pub hash: u64,
    pub ty: CType,
    pub rbp_off: i32
}

pub struct LocalTable {
    locals: SmallVec<[Local; MAX_LOCALS]>,
    pub frame_bytes: i32,
}

impl LocalTable {
    #[inline]
    pub fn new() -> Self {
        Self {
            locals: SmallVec::new(),
            frame_bytes: 0
        }
    }

    #[inline]
    pub fn alloc(&mut self, hash: u64, ty: CType) -> i32 {
        self.frame_bytes += ty.size() as i32;
        let rbp_off = -(self.frame_bytes);
        self.locals.push(Local { hash, ty, rbp_off });
        rbp_off
    }

    #[inline]
    pub fn find(&self, hash: u64) -> Option<Local> {
        self.locals.iter().rev().find(|var| var.hash == hash).copied()
    }
}

bitflags::bitflags! {
    #[derive(Clone, Copy)]
    pub struct SymFlags: u8 {
        const DEFINED  = 0x1;
        const EXTERN   = 0x2;
        const VARIADIC = 0x4;
    }
}

#[derive(Clone, Copy)]
pub struct Symbol {
    pub hash:        u64,

    pub code_off:    u32,
    pub code_len:    u32,

    pub name_off:    u32,
    pub name_len:    u16,

    // For procedures
    pub param_count: u8,
    pub ret_ty:      CType,

    pub flags:       SymFlags
}

impl Symbol {
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

pub struct SymTable {
    pub syms:     SmallVec<[Symbol; 64]>,
    index:        IntMap<u64, u32>,
    pub name_buf: Vec<u8>,
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
            index: IntMap::with_capacity_and_hasher(4096, Default::default()),
            name_buf: Vec::with_capacity(4096)
        }
    }

    #[inline]
    pub fn find(&self, hash: u64) -> Option<usize> {
        self.index.get(&hash).map(|&i| i as usize)
    }

    #[inline]
    pub fn insert(
        &mut self,
        name: &str,
        code_off: u32, code_len: u32,
        flags: SymFlags,
        param_count: Option<u8>,
        ret_ty: Option<CType>
    ) -> usize {
        let hash = fnv1a_str(name);
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
            hash,
            name_off, name_len: name.len() as u16,
            code_off, code_len,
            param_count: param_count.unwrap_or(0),
            ret_ty: ret_ty.unwrap_or(CType::Void),
            flags,
        });
        self.index.insert(hash, i as u32);

        i
    }

    #[inline]
    pub fn s(&self, i: usize) -> &str {
        self.syms[i].s(&self.name_buf)
    }
}

#[derive(Clone, Copy)]
pub struct Reloc {
    pub offset: u32,
    pub sym_index: u32,
    pub addend: i64
}

pub struct RodataReloc {
    pub text_off: u32,
    pub rodata_off: u32
}

pub struct Compiler {
    pub pp:            PP,
    pub buf:           CodeBuf,
    pub vstack:        ValueStack,
    pub xmms:          XmmAlloc,
    pub regs:          RegAlloc,
    pub syms:          SymTable,
    pub relocs:        Vec<Reloc>,
    pub rodata:        Vec<u8>,
    pub rodata_relocs: Vec<RodataReloc>,

    // Reset per function
    locals:  LocalTable,
    ret_ty:  CType,
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

impl Compiler {
    #[inline]
    pub fn new(pp: PP) -> Self {
        Self {
            pp,
            buf: CodeBuf::new(), vstack: ValueStack::new(),
            regs: RegAlloc::new(), xmms: XmmAlloc::new(),
            syms: SymTable::new(), relocs: Vec::new(),
            rodata: Vec::new(), rodata_relocs: Vec::new(),
            locals: LocalTable::new(), ret_ty: CType::Void,
        }
    }

    // cur/peek/next come from Deref to PP.
    // eat() - assert kind and advance, or error
    #[inline]
    fn expect(&mut self, kind: TK, what: &'static str) -> CResult<Token> {
        let t = self.current_token;
        if t.kind == kind { self.next(); Ok(t) }
        else { Err(CError::Expected { span: t.span, expected: what, got: self.s(t).to_owned() }) }
    }

    #[inline]
    fn eat_ident(&mut self, what: &'static str) -> CResult<Token> {
        self.expect(TK::Ident, what)
    }

    #[inline]
    fn at_eof(&self) -> bool { self.current_token.kind == TK::Eof }

    #[inline]
    fn compile_type(&mut self) -> CResult<CType> {
        let t = self.eat_ident("type name")?;
        let base = match t.hash {
            HASH_INT  => CType::Int,
            HASH_LONG => CType::Long,
            HASH_CHAR => CType::Char,
            HASH_VOID => CType::Void,
            HASH_FLOAT => CType::Float,
            HASH_DOUBLE => CType::Double,

            _ => return Err(CError::UnknownType {
                span: t.span,
                name: t.s(&self.pp.src_arena).to_owned()
            }),
        };

        let mut depth = 0u8;
        while self.current_token.kind == TK::Star {
            self.next();
            depth += 1;
        }

        if depth > 0 { Ok(CType::Ptr(depth)) } else { Ok(base) }
    }

    // Materialize a CValue into a register
    #[inline]
    fn force_gp(&mut self, v: CValue) -> CResult<Reg> {
        match v.kind {
            VK::Reg => match v.reg {
                ValReg::Gp(r) => Ok(r),
                ValReg::Xmm(_) => unreachable!("float in force_reg"),
            },
            VK::Imm => {
                let r = self.regs.alloc(Span::POISONED)?;
                self.buf.mov_ri64(r, v.imm);
                Ok(r)
            }
            VK::Local | VK::RegInd => {
                let r = self.regs.alloc(Span::POISONED)?;
                self.buf.mov_load(r, v.reg.as_gp(), v.offset, v.ty.is64());
                Ok(r)
            }
        }
    }

    // Materialize a CValue into an XMM register
    #[inline]
    fn force_xmm(&mut self, v: CValue) -> CResult<XmmReg> {
        match v.kind {
            VK::Reg => match v.reg {
                ValReg::Xmm(r) => Ok(r),
                ValReg::Gp(_) => unreachable!("int in force_xmm"),
            },

            VK::Imm => {
                // Float immediate - store in rodata and load

                let xmm = self.xmms.alloc(Span::POISONED)?;
                let rodata_off = self.rodata.len() as u32;
                match v.ty {
                    CType::Float  => self.rodata.extend_from_slice(&(v.fimm as f32).to_bits().to_le_bytes()),
                    _             => self.rodata.extend_from_slice(&v.fimm.to_bits().to_le_bytes()),
                }
                let text_off = match v.ty {
                    CType::Float => self.buf.movss_load_rip(xmm),
                    _            => self.buf.movsd_load_rip(xmm),
                } as _;
                self.rodata_relocs.push(RodataReloc { text_off, rodata_off });

                Ok(xmm)
            }

            VK::Local | VK::RegInd => {
                let xmm = self.xmms.alloc(Span::POISONED)?;
                let base = match v.reg { ValReg::Gp(r) => r, _ => unreachable!() };
                match v.ty {
                    CType::Float => self.buf.movss_load(xmm, base, v.offset),
                    _            => self.buf.movsd_load(xmm, base, v.offset),
                }
                Ok(xmm)
            }
        }
    }

    // in compile_binop float path, after force_xmm calls:
    #[inline]
    fn coerce_to_xmm(&mut self, v: CValue, target_ty: CType) -> CResult<XmmReg> {
        if v.ty.is_float() {
            let r = self.force_xmm(v)?;

            // Still need to convert if types differ
            if v.ty == CType::Double && target_ty == CType::Float {
                self.buf.cvtsd2ss(r, r);
            } else if v.ty == CType::Float && target_ty == CType::Double {
                self.buf.cvtss2sd(r, r);
            }

            return Ok(r);
        }

        //
        // int -> float
        //
        let gp = self.force_gp(v)?;
        let xmm = self.xmms.alloc(Span::POISONED)?;
        self.buf.cvtsi2sd(xmm, gp);  // Always convert to double first
        self.regs.free(gp);
        if target_ty == CType::Float {
            self.buf.cvtsd2ss(xmm, xmm);
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
    fn pop_reg(&mut self) -> CResult<(Reg, CType)> {
        let v = self.vstack.pop();
        Ok((self.force_gp(v)?, v.ty))
    }

    #[inline]
    fn pop_xmm(&mut self) -> CResult<(XmmReg, CType)> {
        let v = self.vstack.pop();
        Ok((self.force_xmm(v)?, v.ty))
    }

    #[inline]
    pub fn compile(&mut self) {
        while !self.at_eof() {
            if let Err(e) = self.compile_top_level() {
                e.emit(&self.pp.src_arena); std::process::exit(1);
            }
        }
    }

    fn compile_top_level(&mut self) -> CResult<()> {
        let is_extern = self.current_token.kind == TK::Ident &&
                        self.current_token.hash == HASH_EXTERN;
        if is_extern { self.next(); }

        let ret_ty   = self.compile_type()?;
        let name_tok = self.eat_ident("function or variable name")?;
        let name     = self.s(name_tok).to_owned();
        let hash     = name_tok.hash;

        if self.current_token.kind != TK::LParen {
            // Global variable - skip for now
            while !matches!(self.current_token.kind, TK::SemiColon | TK::Eof) {
                self.next();
            }
            self.expect(TK::SemiColon, "';'")?;

            return Ok(());
        }

        self.next(); // '('
        let (params, is_variadic) = self.compile_params()?;
        self.expect(TK::RParen, "')'")?;

        let mut flags = SymFlags::empty();
        flags.set(SymFlags::VARIADIC, is_variadic);

        if is_extern || self.current_token.kind == TK::SemiColon {
            flags.insert(SymFlags::EXTERN);

            self.expect(TK::SemiColon, "';'")?;
            self.syms.insert(&name, 0, 0, flags, Some(params.len() as _), Some(ret_ty));
            return Ok(());
        }

        flags.insert(SymFlags::DEFINED);
        self.compile_func(&name, hash, ret_ty, params, flags)
    }

    fn compile_func(
        &mut self,
        name: &str,
        _hash: u64,
        ret_ty: CType,
        params: Vec<(CType,u64)>,
        flags: SymFlags
    ) -> CResult<()> {
        self.locals = LocalTable::new();
        self.regs   = RegAlloc::new();
        self.ret_ty = ret_ty;

        let code_off = self.buf.pos() as u32;
        let mut code_len = 0;

        //
        // Code len as 0 for now (uncompiled)
        //

        let sym_index = self.syms.insert(
            name,
            code_off,
            code_len,
            flags,
            Some(params.len() as _),
            Some(ret_ty)
        );

        //
        // Prologue
        //
        self.buf.push_r(Reg::Rbp);
        self.buf.mov_rr(Reg::Rbp, Reg::Rsp);
        let frame_patch = self.buf.pos() + 3; // imm32 inside sub rsp, imm32
        self.buf.sub_rsp(0);

        //
        // Spill args to stack (convenience, for pop_reg)
        //
        for (i, (ty, phash)) in params.iter().enumerate() {
            let off = self.locals.alloc(*phash, *ty);
            self.buf.mov_store(Reg::Rbp, off, ARG_REGS[i], ty.is64());
        }

        self.expect(TK::LCurly, "'{'")?;
        while self.current_token.kind != TK::RCurly && !self.at_eof() {
            if let Err(e) = self.compile_stmt() {
                e.emit(&self.pp.src_arena);
                self.recover();
            }
        }
        self.expect(TK::RCurly, "'}'")?;

        //
        // Fall-through epilogue (handles void / implicit return)
        //
        self.buf.mov_rr(Reg::Rsp, Reg::Rbp);
        self.buf.pop_r(Reg::Rbp);
        self.buf.ret();

        let frame = (self.locals.frame_bytes + 15) & !15;
        self.buf.patch_i32(frame_patch, frame);

        //
        // Patch up the code length
        //
        code_len = self.buf.bytes.len() as u32 - code_off;
        self.syms[sym_index].code_len = code_len;

        Ok(())
    }

    #[inline]
    fn compile_params(&mut self) -> CResult<(Vec<(CType,u64)>, bool)> {
        let mut params = Vec::new();
        if self.current_token.kind == TK::RParen {
            return Ok((params, false));
        }

        if  self.current_token.kind == TK::Ident &&
            self.current_token.hash == HASH_VOID &&
            self.next_token.kind    == TK::RParen
        {
            self.next(); return Ok((params, false));
        }

        let mut variadic = false;
        loop {
            let ty   = self.compile_type()?;
            let hash = if self.current_token.kind == TK::Ident {
                let t = self.next();
                t.hash
            } else {
                0
            };

            params.push((ty, hash));

            if self.current_token.kind != TK::Comma { break; }
            self.next();
            if self.current_token.kind == TK::TripleDot { variadic = true; self.next(); break; }
        }

        Ok((params, variadic))
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
                else if HASH_TYPES.contains(&h) { self.compile_local_decl() }
                else                            { self.compile_expr_stmt()  }
            }

            TK::LCurly => self.compile_block(),

            _          => self.compile_expr_stmt(),
        }
    }

    #[inline]
    fn compile_block(&mut self) -> CResult<()> {
        self.expect(TK::LCurly, "'{'")?;
        while self.current_token.kind != TK::RCurly && !self.at_eof() {
            if let Err(e) = self.compile_stmt() { e.emit(&self.pp.src_arena); self.recover(); }
        }
        self.expect(TK::RCurly, "'}'").map(|_| ())
    }

    #[inline]
    fn compile_return(&mut self) -> CResult<()> {
        self.next(); // return

        if self.current_token.kind != TK::SemiColon {
            self.compile_expr()?;

            let ret_ty = self.ret_ty;
            if ret_ty.is_float() {
                let v = self.vstack.pop();
                let r = self.coerce_to_xmm(v, ret_ty)?;

                match self.ret_ty {
                    CType::Float => self.buf.movss_rr(XmmReg::Xmm0, r),
                    _            => self.buf.movsd_rr(XmmReg::Xmm0, r),
                }

                self.xmms.free(r);
            } else {
                let (r, _) = self.pop_reg()?;

                self.buf.mov_rr(Reg::Rax, r);
                self.regs.free(r);
            }
        }

        self.expect(TK::SemiColon, "';'")?;
        self.buf.mov_rr(Reg::Rsp, Reg::Rbp);
        self.buf.pop_r(Reg::Rbp);
        self.buf.ret();

        Ok(())
    }

    #[inline]
    fn compile_if(&mut self) -> CResult<()> {
        self.next(); // if

        self.expect(TK::LParen, "'('")?; // (
        self.compile_expr()?;
        self.expect(TK::RParen, "')'")?; // )

        let (r, _) = self.pop_reg()?;
        self.buf.test_rr(r);
        self.free_reg(ValReg::Gp(r));

        let je_patch = self.buf.je_rel32(); // je .else_or_end

        self.compile_stmt()?; // then

        if self.current_token.kind == TK::Ident && self.current_token.hash == HASH_ELSE {
            self.next(); // else
            let jmp_patch = self.buf.jmp_rel32(); // jmp .end
            self.buf.patch_rel32(je_patch, self.buf.pos());
            self.compile_stmt()?;  // else-body
            self.buf.patch_rel32(jmp_patch, self.buf.pos());
        } else {
            self.buf.patch_rel32(je_patch, self.buf.pos());
        }

        Ok(())
    }

    fn compile_while(&mut self) -> CResult<()> {
        self.next(); // while
        self.expect(TK::LParen, "'('")?;

        //
        // Collect cond tokens
        //
        let mut cond_toks = Vec::new();
        while self.current_token.kind != TK::RParen && !self.at_eof() {
            cond_toks.push(self.next());
        }
        self.expect(TK::RParen, "')'")?;

        //
        // Jmp to cond
        //
        let jmp_cond = self.buf.jmp_rel32();
        let loop_top = self.buf.pos();

        //
        // Body
        //
        self.compile_stmt()?;

        //
        // Cond
        //
        self.buf.patch_rel32(jmp_cond, self.buf.pos());
        if !cond_toks.is_empty() {
            let saved_cur  = self.pp.current_token;
            let saved_peek = self.pp.next_token;

            let mut replay = cond_toks;
            replay.push(saved_cur);
            replay.push(saved_peek);

            self.pp.exp.push(replay.as_slice());
            self.pp.current_token = self.pp.cook();
            self.pp.next_token    = self.pp.cook();

            self.compile_expr()?;
            let (r, _) = self.pop_reg()?;
            self.buf.test_rr(r);
            self.regs.free(r);
        }

        let jne_patch = self.buf.jne_rel32();
        self.buf.patch_rel32(jne_patch, loop_top);

        Ok(())
    }

    fn compile_for(&mut self) -> CResult<()> {
        self.next(); // for
        self.expect(TK::LParen, "'('")?;

        //
        // Init
        //
        if self.current_token.kind != TK::SemiColon {
            let h = self.current_token.hash;
            if HASH_TYPES.contains(&h) {
                self.compile_local_decl()?;
            } else {
                self.compile_expr()?;
                let v = self.vstack.pop();
                if v.kind == VK::Reg { self.free_reg(v.reg); }
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
        let jmp_cond = self.buf.jmp_rel32();
        let loop_top = self.buf.pos();

        //
        // Body
        //
        self.compile_stmt()?;

        //
        // Post
        //
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
            if v.kind == VK::Reg { self.free_reg(v.reg); }
        }

        //
        // Cond
        //
        self.buf.patch_rel32(jmp_cond, self.buf.pos());
        if !cond_toks.is_empty() {
            let saved_cur  = self.pp.current_token;
            let saved_peek = self.pp.next_token;

            let mut replay = cond_toks;
            replay.push(saved_cur);
            replay.push(saved_peek);

            self.pp.exp.push(replay.as_slice());
            self.pp.current_token = self.pp.cook();
            self.pp.next_token    = self.pp.cook();

            self.compile_expr()?;
            let (r, _) = self.pop_reg()?;
            self.buf.test_rr(r);
            self.regs.free(r);
        }

        let jne_patch = self.buf.jne_rel32();
        self.buf.patch_rel32(jne_patch, loop_top);

        Ok(())
    }

    #[inline]
    fn compile_store_impl(&mut self, base: Reg, off: i32, ty: CType, keep: bool) -> CResult<()> {
        if !ty.is_float() {
            let (r, _) = self.pop_reg()?;
            self.buf.mov_store(base, off, r, ty.is64());

            if keep {
                self.vstack.push(CValue::gp(ty, r));
            } else {
                self.regs.free(r);
            }

            return Ok(());
        }

        let v = self.vstack.pop();
        let r = self.coerce_to_xmm(v, ty)?;

        match ty {
            CType::Float => self.buf.movss_store(base, off, r),
            _            => self.buf.movsd_store(base, off, r),
        }

        if keep {
            self.vstack.push(CValue::xmm(ty, r));
        } else {
            self.xmms.free(r);
        }

        Ok(())
    }

    #[inline]
    fn compile_store(&mut self, base: Reg, off: i32, ty: CType) -> CResult<()> {
        self.compile_store_impl(base, off, ty, false)
    }

    #[inline]
    fn compile_store_keep(&mut self, base: Reg, off: i32, ty: CType) -> CResult<()> {
        self.compile_store_impl(base, off, ty, true)
    }

    #[inline]
    fn compile_local_decl(&mut self) -> CResult<()> {
        let ty       = self.compile_type()?;
        let name_tok = self.eat_ident("variable name")?;
        let hash     = name_tok.hash;
        let off      = self.locals.alloc(hash, ty);
        if self.current_token.kind == TK::Eq {
            self.next();
            self.compile_expr()?;
            self.compile_store(Reg::Rbp, off, ty)?;
        }
        self.expect(TK::SemiColon, "';'").map(|_| ())
    }

    #[inline]
    fn compile_expr_stmt(&mut self) -> CResult<()> {
        self.compile_expr()?;
        let v = self.vstack.pop();
        if v.kind == VK::Reg { self.free_reg(v.reg); }
        self.expect(TK::SemiColon, "';'").map(|_| ())
    }

    #[inline]
    fn recover(&mut self) {
        loop {
            match self.current_token.kind {
                TK::Eof | TK::RCurly => break,
                TK::SemiColon => { self.next(); break; }
                _ => { self.next(); }
            }
        }
    }

    // -- Expressions -------------------------------------------
    //
    // prec table (low -> high):
    //   1  = += -= *= /= &= |=  (right-assoc)
    //   2  == !=
    //   3  < > <= >=
    //   4  + -
    //   5  * /
    //   prefix:  - & *
    //   primary: NUMBER IDENT CALL STRING ( expr )

    #[inline]
    fn compile_expr(&mut self) -> CResult<()> {
        self.compile_expr_impl(0)
    }

    #[inline]
    const fn op_prec(k: TK) -> Option<(u8, bool)> {
        match k {
            TK::Eq   | TK::PlusEq | TK::MinusEq | TK::StarEq | TK::SlashEq => Some((1, true)),
            TK::EqEq | TK::NotEq                           => Some((2, false)),
            TK::Less | TK::Greater | TK::LessEq | TK::GreaterEq => Some((3, false)),
            TK::Plus | TK::Minus                           => Some((4, false)),
            TK::Star | TK::Slash                           => Some((5, false)),
            _                                              => None,
        }
    }

    #[inline]
    fn compile_expr_impl(&mut self, min_prec: u8) -> CResult<()> {
        // Fast path: no prefix operator - go straight to primary
        match self.current_token.kind {
            TK::Minus | TK::BinAnd | TK::Star => self.compile_unary()?,
            _ => self.compile_primary()?,
        }

        loop {
            let (prec, right) = match Self::op_prec(self.current_token.kind) {
                Some(p) if p.0 >= min_prec => p,
                _ => break,
            };

            let op   = self.current_token.kind;
            let span = self.current_token.span;
            self.next();

            if matches!(op, TK::Eq | TK::PlusEq | TK::MinusEq | TK::StarEq | TK::SlashEq) {
                let lhs = self.vstack.pop();
                if !lhs.is_lvalue() { return Err(CError::NotLvalue { span }); }

                self.compile_expr_impl(if right { prec } else { prec + 1 })?;

                if op != TK::Eq {
                    // rhs is on top - pop it, load lhs value, push both back in order
                    let rhs = self.vstack.pop();
                    let base = lhs.reg.as_gp();

                    if lhs.ty.is_float() {
                        let tmp = self.xmms.alloc(span)?;
                        match lhs.ty {
                            CType::Float => self.buf.movss_load(tmp, base, lhs.offset),
                            _            => self.buf.movsd_load(tmp, base, lhs.offset),
                        }
                        self.vstack.push(CValue::xmm(lhs.ty, tmp));
                    } else {
                        let tmp = self.regs.alloc(span)?;
                        self.buf.mov_load(tmp, base, lhs.offset, lhs.ty.is64());
                        self.vstack.push(CValue::gp(lhs.ty, tmp));
                    }

                    self.vstack.push(rhs);
                    let arith_op = match op {
                        TK::PlusEq  => TK::Plus,
                        TK::MinusEq => TK::Minus,
                        TK::StarEq  => TK::Star,
                        TK::SlashEq => TK::Slash,
                        _ => unreachable!(),
                    };
                    self.compile_binop(arith_op, span)?;
                }

                let base = lhs.reg.as_gp();
                self.compile_store_keep(base, lhs.offset, lhs.ty)?;
            } else {
                self.compile_expr_impl(if right { prec } else { prec + 1 })?;
                self.compile_binop(op, span)?;
            }
        }

        Ok(())
    }

    #[inline]
    fn compile_binop(&mut self, op: TK, span: Span) -> CResult<()> {
        match op {
            TK::Plus | TK::Minus | TK::Star | TK::Slash => {
                let rhs = self.vstack.pop();
                let lhs = self.vstack.pop();

                if lhs.ty.is_float() {
                    let target_ty = if lhs.ty == CType::Double || rhs.ty == CType::Double {
                        CType::Double
                    } else {
                        CType::Float
                    };
                    let l = self.coerce_to_xmm(lhs, target_ty)?;
                    let r = self.coerce_to_xmm(rhs, target_ty)?;

                    let ty = self.normalize_xmm(l, lhs.ty, r, rhs.ty);
                    match (op, ty) {
                        (TK::Plus,  CType::Float)  => self.buf.addss(l, r),
                        (TK::Plus,  _)             => self.buf.addsd(l, r),
                        (TK::Minus, CType::Float)  => self.buf.subss(l, r),
                        (TK::Minus, _)             => self.buf.subsd(l, r),
                        (TK::Star,  CType::Float)  => self.buf.mulss(l, r),
                        (TK::Star,  _)             => self.buf.mulsd(l, r),
                        (TK::Slash, CType::Float)  => self.buf.divss(l, r),
                        (TK::Slash, _)             => self.buf.divsd(l, r),
                        _ => unreachable!(),
                    }

                    self.xmms.free(r);
                    self.vstack.push(CValue::xmm(ty, l));

                    return Ok(());
                }

                let (lhs, ty) = (self.force_gp(lhs)?, lhs.ty);
                let rhs = self.force_gp(rhs)?;
                match op {
                    TK::Plus  => { self.buf.add_rr(lhs, rhs);  self.regs.free(rhs); self.vstack.push(CValue::gp(ty, lhs)); }
                    TK::Minus => { self.buf.sub_rr(lhs, rhs);  self.regs.free(rhs); self.vstack.push(CValue::gp(ty, lhs)); }
                    TK::Star  => { self.buf.imul_rr(lhs, rhs); self.regs.free(rhs); self.vstack.push(CValue::gp(ty, lhs)); }
                    TK::Slash => {
                        self.buf.mov_rr(Reg::Rax, lhs);
                        self.buf.cqo();

                        let actual = if rhs == Reg::Rdx {
                            let t = self.regs.alloc(span)?;
                            self.buf.mov_rr(t, Reg::Rdx);
                            t
                        } else {
                            rhs
                        };

                        self.buf.idiv_r(actual);

                        self.regs.free(lhs);
                        self.regs.free(actual);
                        if actual != rhs { self.regs.free(rhs); }
                        self.regs.mark(Reg::Rax);
                        self.vstack.push(CValue::gp(ty, Reg::Rax));
                    }

                    _ => unreachable!(),
                }
            }

            TK::EqEq | TK::NotEq | TK::Less | TK::Greater | TK::LessEq | TK::GreaterEq =>
                self.compile_cmp(op)?,

            _ => unreachable!(),
        }

        Ok(())
    }

    // Promote float->double if types differ. Returns the common type.
    #[inline]
    fn normalize_xmm(&mut self, l: XmmReg, lty: CType, r: XmmReg, rty: CType) -> CType {
        if lty == rty { return lty; }

        // One is float, one is double - promote float to double
        if lty == CType::Float { self.buf.cvtss2sd(l, l); }
        if rty == CType::Float { self.buf.cvtss2sd(r, r); }

        CType::Double
    }

    #[inline]
    fn compile_cmp(&mut self, op: TK) -> CResult<()> {
        let rhs = self.vstack.pop();
        let lhs = self.vstack.pop();

        if lhs.ty.is_float() {
            let l = self.force_xmm(lhs)?;
            let r = self.force_xmm(rhs)?;

            let ty = self.normalize_xmm(l, lhs.ty, r, rhs.ty);

            match ty {
                CType::Float => self.buf.ucomiss(l, r),
                _            => self.buf.ucomisd(l, r),
            }

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
            self.buf.setcc(dst, setcc);

            self.buf.movzx_rr(dst, dst);
            self.vstack.push(CValue::gp(CType::Int, dst));
        } else {
            let r = self.force_gp(rhs)?;
            let l = self.force_gp(lhs)?;
            self.buf.cmp_rr(l, r);

            let setcc: u8 = match op {
                TK::EqEq      => 0x94,
                TK::NotEq     => 0x95,
                TK::Less      => 0x9C,
                TK::Greater   => 0x9F,
                TK::LessEq    => 0x9E,
                TK::GreaterEq => 0x9D,
                _ => unreachable!(),
            };
            self.buf.setcc(l, setcc);
            self.buf.movzx_rr(l, l);

            self.regs.free(r);
            self.vstack.push(CValue::gp(CType::Int, l));
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
                if !v.ty.is_float() {
                    let (r, ty) = self.pop_reg()?;
                    self.buf.neg_r(r);
                    self.vstack.push(CValue::gp(ty, r));
                    return Ok(());
                }

                let (r, ty) = self.pop_xmm()?;

                // xorpd xmm, [rip + sign_mask]  - flip sign bit
                let rodata_off = self.rodata.len() as u32;
                match ty {
                    CType::Float  => self.rodata.extend_from_slice(&0x80000000u32.to_le_bytes()),
                    _             => self.rodata.extend_from_slice(&0x8000000000000000u64.to_le_bytes()),
                }

                let text_off = match ty {
                    CType::Float => self.buf.xorps_rip(r),
                    _            => self.buf.xorpd_rip(r),
                } as _;

                self.rodata_relocs.push(RodataReloc { text_off, rodata_off });
                self.vstack.push(CValue::xmm(ty, r));
            }

            TK::BinAnd => {
                let span = self.current_token.span; self.next();
                self.compile_primary()?;

                let v = self.vstack.pop();
                if !v.is_lvalue() { return Err(CError::NotLvalue { span }); }

                let base = v.reg.as_gp();

                let dst = self.regs.alloc(span)?;
                self.buf.lea(dst, base, v.offset);
                self.vstack.push(CValue::gp(CType::Ptr(1), dst));
            }

            TK::Star => {
                self.next();
                self.compile_unary()?;

                let v  = self.vstack.pop();
                let r  = self.force_gp(v)?;

                let ty = match v.ty {
                    CType::Ptr(d) if d > 1 => CType::Ptr(d-1),
                    CType::Ptr(_)          => CType::Long,
                    other => other
                };

                self.vstack.push(CValue::regind(ty, r, 0));
            }

            _ => self.compile_primary()?,
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
                    let v: i64 = s.parse().unwrap_or(0);
                    self.vstack.push(CValue::imm(CType::Int, v));
                    return Ok(())
                }

                //
                // float literal - store bits in rodata, load via movsd/movss
                //

                let is_float = s.ends_with('f');
                let num_str = if is_float { &s[..s.len()-1] } else { s };
                let v: f64 = num_str.parse().unwrap_or(0.0);
                let ty = if is_float { CType::Float } else { CType::Double };

                let rodata_off = self.rodata.len() as u32;
                if is_float {
                    self.rodata.extend_from_slice(&(v as f32).to_bits().to_le_bytes());
                } else {
                    self.rodata.extend_from_slice(&v.to_bits().to_le_bytes());
                }

                let xmm = self.xmms.alloc(t.span)?;
                let text_off = if is_float {
                    self.buf.movss_load_rip(xmm)
                } else {
                    self.buf.movsd_load_rip(xmm)
                } as _;

                self.rodata_relocs.push(RodataReloc { text_off, rodata_off });
                self.vstack.push(CValue::xmm(ty, xmm));
            }

            TK::StrLit => {
                let t   = self.next();
                let raw = t.s(&self.pp.src_arena);
                let s   = &raw[1..raw.len()-1];
                let rodata_off = self.rodata.len() as u32;

                // @Refactor: Put this into a separate function

                //
                // Unescape into rodata
                //
                let bytes = s.as_bytes(); let mut i = 0;
                while i < bytes.len() {
                    if bytes[i] == b'\\' && i+1 < bytes.len() {
                        i += 1;
                        self.rodata.push(match bytes[i] {
                            b'n' => b'\n', b't' => b'\t', b'r' => b'\r',
                            b'0' => b'\0', other => other,
                        });
                    } else {
                        self.rodata.push(bytes[i]);
                    }

                    i += 1;
                }
                self.rodata.push(0);

                // lea dst, [rip + disp32] - displacement patched by write_elf via R_X86_64_PC32
                let dst = self.regs.alloc(Span::POISONED)?;
                let patch = self.buf.lea_rip(dst);
                self.rodata_relocs.push(RodataReloc { text_off: patch as _, rodata_off });

                self.vstack.push(CValue::gp(CType::Ptr(1), dst));
            }

            TK::Ident => {
                let name_tok = self.next();
                let hash     = name_tok.hash;
                if self.current_token.kind == TK::LParen {
                    self.compile_call(hash, name_tok)?;
                } else if let Some(lv) = self.locals.find(hash) {
                    self.vstack.push(CValue::local(lv.ty, lv.rbp_off));
                } else {
                    return Err(CError::Undefined {
                        span: name_tok.span,
                        name: self.s(name_tok).to_owned()
                    });
                }
            }

            TK::LParen => {
                self.next();
                self.compile_expr()?;
                self.expect(TK::RParen, "')'")?;
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

        Ok(())
    }

    // -- call - SysV AMD64 ABI ------------------------------------------------
    //   eval args -> rdi rsi rdx rcx r8 r9
    //   xor eax, eax  (al = SSE arg count = 0, if callee is variadic)
    //   call rel32
    //   result in rax

    fn compile_call(&mut self, callee_hash: u64, name_tok: Token) -> CResult<()> {
        self.next(); // '('

        let mut argc     = 0;
        let mut xmm_argc = 0;

        let Some(sym_index) = self.syms.find(callee_hash) else {
            return Err(CError::Undefined {
                span: name_tok.span,
                name: self.s(name_tok).to_owned()
            });
        };
        let sym = self.syms[sym_index];
        let is_variadic = sym.flags.contains(SymFlags::VARIADIC);

        while self.current_token.kind != TK::RParen && !self.at_eof() {
            self.compile_expr()?;
            let v = self.vstack.pop();

            if v.ty.is_float() {
                if xmm_argc >= XMM_ARG_REGS.len() {
                    return Err(CError::ArgumentCountMismatch {
                        span: self.current_token.span,
                        expected: XMM_ARG_REGS.len(),
                        name: name_tok.s(&self.src_arena).to_owned()
                    });
                }

                let src = self.force_xmm(v)?;
                let dst = XMM_ARG_REGS[xmm_argc];

                //
                // Varargs: promote float -> double (SYSV)
                //

                if is_variadic && v.ty == CType::Float {
                    self.buf.cvtss2sd(dst, src);
                } else {
                    self.buf.movsd_rr(dst, src);
                }

                self.xmms.free(src);

                xmm_argc += 1;
            } else {
                if argc >= ARG_REGS.len() {
                    return Err(CError::ArgumentCountMismatch {
                        span: self.current_token.span,
                        expected: ARG_REGS.len(),
                        name: name_tok.s(&self.src_arena).to_owned()
                    });
                }

                let r = self.force_gp(v)?;
                self.buf.mov_rr(ARG_REGS[argc], r);
                self.regs.free(r);

                argc += 1;
            }

            if self.current_token.kind == TK::Comma {
                self.next();
            } else {
                break;
            }
        }

        let rparen = self.expect(TK::RParen, "')'")?;
        let call_span = name_tok.span.merge(rparen.span);
        let total_argc = argc + xmm_argc;

        //
        // Check the counts match
        //
        if sym.flags.contains(SymFlags::VARIADIC) {
            if total_argc < sym.param_count as usize {
                return Err(CError::ArgumentCountMismatch {
                    span: call_span,
                    expected: sym.param_count as _,
                    name: name_tok.s(&self.src_arena).to_owned()
                });
            }
        } else if total_argc != sym.param_count as usize {
            return Err(CError::ArgumentCountMismatch {
                span: call_span,
                expected: sym.param_count as _,
                name: name_tok.s(&self.src_arena).to_owned()
            });
        }

        // al = number of xmm args used (SYSV)
        if is_variadic {
            if xmm_argc == 0 {
                self.buf.xor_rr(Reg::Rax, Reg::Rax);
            } else {
                self.buf.mov_ri64(Reg::Rax, xmm_argc as i64);
            }
        }

        let call_site = self.buf.call_rel32();
        if sym.flags.contains(SymFlags::EXTERN) {
            self.relocs.push(Reloc {
                offset: call_site as u32,
                sym_index: sym_index as u32,
                addend: -4
            });
        } else {
            self.buf.patch_call(call_site, sym.code_off as usize);
        }

        self.regs.clobber_caller_save();
        self.xmms.clobber_caller_save();

        if sym.ret_ty.is_float() {
            self.xmms.mark(XmmReg::Xmm0);
            self.vstack.push(CValue::xmm(sym.ret_ty, XmmReg::Xmm0));
        } else {
            self.regs.mark(Reg::Rax);
            self.vstack.push(CValue::gp(sym.ret_ty, Reg::Rax));
        }

        Ok(())
    }
}

// --- ELF64 writer ------------------------------------------------------------
//
// Sections:
//   [0] null   [1] .text   [2] .rodata   [3] .rela.text
//   [4] .symtab  [5] .strtab  [6] .shstrtab
//
// Symbols:
//   [0] null sentinel   [1] .rodata STT_SECTION (for PC32 relocs)
//   [2..] defined globals   [...] extern (undefined) globals

pub fn write_elf(c: &Compiler) -> Vec<u8> {
    let nsyms = c.syms.len();

    // strtab
    let mut strtab = Vec::with_capacity(c.syms.name_buf.len() + nsyms); // +1 null per sym
    strtab.push(0u8);
    let mut sym_name_index = Vec::with_capacity(nsyms);
    for sym in c.syms.iter() {
        sym_name_index.push(strtab.len() as u32);
        strtab.extend_from_slice(sym.s(&c.syms.name_buf).as_bytes());
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
    let sh_rela     = sname(".rela.text");
    let sh_symtab   = sname(".symtab");
    let sh_strtab   = sname(".strtab");
    let sh_shstrtab = sname(".shstrtab");

    //
    // symtab
    //

    const SHN_UNDEF:  u16 = 0;
    const SHN_TEXT:   u16 = 1;
    const SHN_RODATA: u16 = 2;
    const STB_LOCAL:  u8  = 0;
    const STB_GLOBAL: u8  = 1;
    const STT_FUNC:   u8  = 2;
    const STT_NOTYPE: u8  = 0;
    const STT_SECTION:u8  = 3;

    let mut symtab = Vec::with_capacity((nsyms + 2) * 24);
    let push_sym = |symtab: &mut Vec<u8>, name: u32, info: u8, shndx: u16, value: u64| {
        symtab.extend_from_slice(&name.to_le_bytes());
        symtab.push(info); symtab.push(0);
        symtab.extend_from_slice(&shndx.to_le_bytes());
        symtab.extend_from_slice(&value.to_le_bytes());
        symtab.extend_from_slice(&0u64.to_le_bytes());
    };

    push_sym(&mut symtab, 0, 0, SHN_UNDEF, 0);  // [0] null
    push_sym(&mut symtab, 0, (STB_LOCAL<<4)|STT_SECTION, SHN_RODATA, 0); // [1] .rodata

    for (i, sym) in c.syms.iter().enumerate() {
        if !sym.flags.contains(SymFlags::DEFINED) { continue; }
        push_sym(&mut symtab, sym_name_index[i], (STB_GLOBAL<<4)|STT_FUNC, SHN_TEXT, sym.code_off as u64);
    }

    let mut elf_sym_index = vec![0u32; c.syms.len()];
    for (i, sym) in c.syms.iter().enumerate() {
        if !sym.flags.contains(SymFlags::EXTERN) { continue; }
        elf_sym_index[i] = (symtab.len() / 24) as u32;
        push_sym(&mut symtab, sym_name_index[i], (STB_GLOBAL<<4)|STT_NOTYPE, SHN_UNDEF, 0);
    }

    // rela.text
    const R_PLT32: u64 = 4;
    const R_PC32:  u64 = 2;
    let mut rela = Vec::with_capacity((c.relocs.len() + c.rodata_relocs.len()) * 24);
    let push_rela = |rela: &mut Vec<u8>, offset: u64, sym: u64, rtype: u64, addend: i64| {
        rela.extend_from_slice(&offset.to_le_bytes());
        rela.extend_from_slice(&((sym<<32)|rtype).to_le_bytes());
        rela.extend_from_slice(&addend.to_le_bytes());
    };

    for r in &c.relocs {
        push_rela(&mut rela, r.offset as u64, elf_sym_index[r.sym_index as usize] as u64, R_PLT32, r.addend);
    }
    for r in &c.rodata_relocs {
        push_rela(&mut rela, r.text_off as u64, 1, R_PC32, r.rodata_off as i64 - 4);
    }

    //
    // Layout
    //

    const EHSZ: usize = 64;
    const SHSZ: usize = 64;
    const NSEC: usize = 7;

    let text_off  = EHSZ;
    let text_sz   = c.buf.bytes.len();
    let rodata_off = align(text_off  + text_sz,   16); let rodata_sz = c.rodata.len();
    let rela_off   = align(rodata_off + rodata_sz,  8); let rela_sz   = rela.len();
    let sym_off    = align(rela_off  + rela_sz,     8); let sym_sz    = symtab.len();
    let str_off    = align(sym_off   + sym_sz,      8); let str_sz    = strtab.len();
    let shstr_off  = align(str_off   + str_sz,      8); let shstr_sz  = shstrtab.len();
    let shdrs_off  = align(shstr_off + shstr_sz,    8);

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
    out[62..64].copy_from_slice(&6u16.to_le_bytes()); // e_shstrndx

    //
    // Section data
    //

    out[text_off  ..text_off  +text_sz  ].copy_from_slice(&c.buf.bytes);
    out[rodata_off..rodata_off+rodata_sz].copy_from_slice(&c.rodata);
    out[rela_off  ..rela_off  +rela_sz  ].copy_from_slice(&rela);
    out[sym_off   ..sym_off   +sym_sz   ].copy_from_slice(&symtab);
    out[str_off   ..str_off   +str_sz   ].copy_from_slice(&strtab);
    out[shstr_off ..shstr_off +shstr_sz ].copy_from_slice(&shstrtab);

    //
    // Section header
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
    const ALLOC:    u64 = 0x2;
    const EXEC:     u64 = 0x4;

    section_header(&mut out, 0, 0,           NULL,     0,            0,                 0,                0, 0, 0,  00);
    section_header(&mut out, 1, sh_text,     PROGBITS, ALLOC|EXEC,   text_off   as u64, text_sz   as u64, 0, 0, 16, 00);
    section_header(&mut out, 2, sh_rodata,   PROGBITS, ALLOC,        rodata_off as u64, rodata_sz as u64, 0, 0, 16, 00);
    section_header(&mut out, 3, sh_rela,     RELA,     0,            rela_off   as u64, rela_sz   as u64, 4, 1, 8,  24);
    section_header(&mut out, 4, sh_symtab,   SYMTAB,   0,            sym_off    as u64, sym_sz    as u64, 5, 2, 8,  24);
    section_header(&mut out, 5, sh_strtab,   STRTAB,   0,            str_off    as u64, str_sz    as u64, 0, 0, 1,  00);
    section_header(&mut out, 6, sh_shstrtab, STRTAB,   0,            shstr_off  as u64, shstr_sz  as u64, 0, 0, 1,  00);

    out
}

fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() < 2 {
        eprintln!("usage: ccrush <file.c> [-o out.o]"); std::process::exit(1);
    }

    let out_path = args.iter()
        .position(|s| s == "-o")
        .and_then(|i| args.get(i+1)).map(|s| s.as_str()).unwrap_or("out.o");

    let pp = match PP::from_path(Path::new(&args[1])) {
        Ok(pp) => pp,
        Err(e) => { e.emit(&SrcArena::new()); std::process::exit(1); }
    };

    let mut c = Compiler::new(pp);
    c.compile();

    if args.contains(&"-run".into()) {
        run_main(c);
        return;
    }

    let elf = write_elf(&c);
    std::fs::write(out_path, &elf).unwrap_or_else(|e| {
        eprintln!("write {out_path}: {e}");
        std::process::exit(1);
    });

    eprintln!("wrote {out_path} ({} bytes text, {} total)", c.buf.bytes.len(), elf.len());
}

fn run_main(mut c: Compiler) {
    use std::ffi::CString;
    use std::os::raw::c_int;

    //
    // Append rodata and patch rodata relocs
    //

    let rodata_base = c.buf.bytes.len();
    c.buf.bytes.extend_from_slice(&c.rodata);

    for r in &c.rodata_relocs {
        let target = rodata_base + r.rodata_off as usize;
        let patch_pos = r.text_off as usize;
        let rel = (target as i64) - (patch_pos as i64 + 4);

        c.buf.patch_i32(patch_pos, rel as i32);
    }

    //
    // Emit trampolines and patch extern relocs
    //
    // Layout: [code][rodata][trampolines]
    // Each trampoline is 12 bytes: 48 B8 <imm64> FF E0
    //

    //
    // Collect unique symbols first to deduplicate trampolines
    //

    let mut trampoline_offsets = Vec::new(); // (buf_offset, sym_addr)
    let mut sym_to_trampoline = IntMap::default();

    for r in &c.relocs {
        let sym_index = r.sym_index as usize;
        if sym_to_trampoline.contains_key(&sym_index) { continue; }

        let sym_name = c.syms[sym_index].s(&c.syms.name_buf);
        let sym_name_c = CString::new(sym_name).unwrap();
        let sym_addr = unsafe {
            libc::dlsym(libc::RTLD_DEFAULT, sym_name_c.as_ptr())
        } as i64;

        if sym_addr == 0 {
            eprintln!("undefined symbol: {sym_name}");
            std::process::exit(1);
        }

        let trampoline_off = c.buf.bytes.len();
        sym_to_trampoline.insert(sym_index, trampoline_off);
        trampoline_offsets.push((trampoline_off, sym_addr));

        c.buf.mov_ri64(Reg::R11, sym_addr);
        c.buf.jmp_r(Reg::R11);
    }

    //
    // Prepare argc, argv, envp
    //

    let args = std::env::args()
        .map(|s| CString::new(s).unwrap())
        .collect::<Vec<_>>();
    let argv = args.iter()
        .map(|s| s.as_ptr() as *const u8)
        .chain([std::ptr::null()])
        .collect::<Vec<_>>();
    let argc = args.len() as i32;

    let env_vars = std::env::vars()
        .map(|(k, v)| CString::new(format!("{k}={v}")).unwrap())
        .collect::<Vec<_>>();
    let envp = env_vars.iter()
        .map(|s| s.as_ptr() as *const u8)
        .chain([std::ptr::null()])
        .collect::<Vec<_>>();

    //
    // Find main
    //

    let main_hash = fnv1a_str("main");
    let Some(main_sym_index) = c.syms.find(main_hash) else {
        eprintln!("program doesn't have a main function to run");
        std::process::exit(1);
    };
    let main_sym = &c.syms[main_sym_index];
    if !main_sym.flags.contains(SymFlags::DEFINED) {
        eprintln!("program doesn't have a DEFINED main function to run");
        std::process::exit(1);
    }
    let main_off = main_sym.code_off as usize;

    //
    // Mmap, patch call sites, then make executable
    //

    let mut mmap = MmapMut::map_anon(c.buf.bytes.len()).unwrap();
    mmap.copy_from_slice(&c.buf.bytes);

    //
    // Patch extern call sites now that we know base
    //

    let base = mmap.as_ptr() as i64;
    for r in &c.relocs {
        let trampoline_off = sym_to_trampoline[&(r.sym_index as usize)];
        let patch_pos = r.offset as usize;
        let call_site = base + patch_pos as i64 + 4;
        let trampoline_addr = base + trampoline_off as i64;
        let rel = (trampoline_addr - call_site) as i32;
        unsafe {
            std::ptr::write_unaligned(
                mmap.as_mut_ptr().add(patch_pos) as *mut i32,
                rel,
            );
        }
    }

    let mmap_exec = mmap.make_exec().unwrap(); // Atomic W->X
    let base = mmap_exec.as_ptr();

    //
    // Call main
    //

    let f: extern "C" fn(c_int, *const *const u8, *const *const u8) -> c_int = unsafe {
        std::mem::transmute(base as usize + main_off)
    };

    let argv_ptr = argv.as_ptr();
    let envp_ptr = envp.as_ptr();

    let result = f(argc, argv_ptr, envp_ptr);
    std::process::exit(result);
}
