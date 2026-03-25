use std::path::Path;

use ccrush::{Compiler, PP, SrcArena, debug_tokens, run_main, write_elf};

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
        .and_then(|i| args.get(i+1)).map_or("out.o", String::as_str);

    let mut pp = match PP::new(&args[1], &args[1..]) {
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

    if args.contains(&"-run".into()) {
        // @Incomplete
        // @Note: This logically should write elf first and then run and exit, but it is what it is..

        println!("[Running main()..]");
        let code = run_main(c);
        println!("[main() exited with code {code}]");

        return;
    }

    let elf = write_elf(&c);
    std::fs::write(out_path, &elf).unwrap_or_else(|e| {
        eprintln!("write {out_path}: {e}");
        std::process::exit(1);
    });

    eprintln!("wrote {out_path} ({} bytes text, {} total)", c.code.bytes.len(), elf.len());
}
