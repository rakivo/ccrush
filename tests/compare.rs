use ccrush::{RunOutput, run_isolated};
use util::diff_run;

use std::{process::{Command, Stdio}, time::{Duration, Instant}};

use tempfile::TempDir;

mod util {
    use imara_diff::{Algorithm, BasicLineDiffPrinter, Diff, InternedInput, UnifiedDiffConfig};

    use super::*;

    pub fn run_clang(src: &str) -> Result<RunOutput, String> {
        let dir = TempDir::new().unwrap();

        let src_path = dir.path().join("test.c");
        let bin_path = dir.path().join("test_bin");

        std::fs::write(&src_path, src).unwrap();

        // Compile
        let status = Command::new("clang")
            .arg("-O0")
            .arg(&src_path)
            .arg("-o")
            .arg(&bin_path)
            // .arg("-fsanitize=undefined")
            // .arg("-fno-sanitize-recover=all")
            .status()
            .map_err(|e| e.to_string())?;

        if !status.success() {
            return Err("clang compile error".into());
        }

        // Run with timeout
        let mut child = Command::new(&bin_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        let start = Instant::now();
        let timeout = Duration::from_millis(200);

        loop {
            if let Some(status) = child.try_wait().unwrap() {
                let out = child.wait_with_output().unwrap();

                return Ok(RunOutput {
                    exit: status.code().unwrap_or(-1),
                    stdout: String::from_utf8_lossy(&out.stdout).to_string(),
                    stderr: String::from_utf8_lossy(&out.stderr).to_string(),
                });
            }

            if start.elapsed() > timeout {
                let _ = child.kill();
                return Err("timeout".into());
            }

            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[inline]
    pub fn normalize(s: &str) -> String {
        s.trim().replace("\r\n", "\n")
    }

    #[inline]
    fn print_diff(
        before: &str,
        after: &str,
        path: &str,
        out: &mut dyn std::fmt::Write
    ) -> std::fmt::Result {
        let input = InternedInput::new(before, after);
        let mut diff = Diff::compute(Algorithm::Histogram, &input);
        diff.postprocess_lines(&input);

        if diff.hunks().next().is_none() {
            return Ok(()); // Empty diff!
        }

        let printer = BasicLineDiffPrinter(&input.interner);
        let unified = diff.unified_diff(&printer, UnifiedDiffConfig::default(), &input);

        writeln!(out, "--- a/{path}")?;
        writeln!(out, "+++ b/{path}")?;
        writeln!(out, "{unified}")?;

        Ok(())
    }

    pub fn diff_run(name: &str, src: &str) {
        // First, try clang
        let clang = match run_clang(src) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Skipping test {name} because clang failed: {e}");
                return; // Exit early, don't run your compiler
            }
        };

        // Only run your compiler if clang succeeded
        let my = match run_isolated(name, src.as_bytes()) {
            Ok(m) => m,
            Err(e) => {
                panic!(
                    "\n=== TEST: {name} ===\n{src}\nMY COMPILER ERROR: {e:?}\nCLANG succeeded",
                );
            }
        };

        // Compare outputs
        save_regression(name, src);

        let a_out = normalize(&my.stdout);
        let b_out = normalize(&clang.stdout);

        if my.exit != clang.exit || a_out != b_out {
            let msg = format!(
                "\n=== TEST: {name} ===\
                 \n=== SOURCE ===\n{src}\n\
                 === EXIT ===\nmy={my_exit} clang={clang_exit}\n\
                 === STDOUT DIFF ===\n{diff}\n\
                 === MY STDERR ===\n{my_stderr}\n\
                 === CLANG STDERR ===\n{clang_stderr}\n",
                my_exit = my.exit,
                clang_exit = clang.exit,

                diff = {
                    let mut s = String::new();
                    print_diff(&a_out, &b_out, name, &mut s).unwrap();
                    s
                },

                my_stderr = my.stderr,
                clang_stderr = clang.stderr,
            );

            panic!("{msg}");
        }
    }

    #[inline]
    pub fn save_regression(name: &str, src: &str) {
        use std::fs;
        use std::time::{SystemTime, UNIX_EPOCH};

        let dir = "tests/regressions";
        fs::create_dir_all(dir).ok();

        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();

        let path = format!("{dir}/{name}_{ts}.c");
        fs::write(&path, src).unwrap();

        eprintln!("saved regression: {path}");
    }

    #[inline]
    #[allow(unused)]
    pub fn gen_expr(depth: u32) -> String {
        if depth == 0 {
            return format!("{}", rand::random::<i32>() % 10);
        }

        let ops = ["+", "-", "*", "/", "%", "&", "|", "^"];
        let op = ops[rand::random::<u64>() as usize % ops.len()];

        format!(
            "({} {} {})",
            gen_expr(depth - 1),
            op,
            gen_expr(depth - 1)
        )
    }

    #[inline]
    #[allow(unused)]
    pub fn gen_stmt(depth: u32) -> String {
        if depth == 0 {
            return format!("return {};", gen_expr(2));
        }

        match rand::random::<u8>() % 3 {
            0 => format!(
                "if ({}) {{ {} }} else {{ {} }}",
                gen_expr(2),
                gen_stmt(depth - 1),
                gen_stmt(depth - 1)
            ),
            1 => format!(
                "int i=0; while(i<3){{ i=i+1; }} return {};",
                gen_expr(2)
            ),
            _ => format!("return {};", gen_expr(3)),
        }
    }

    #[inline]
    #[allow(unused)]
    pub fn gen_program() -> String {
        format!(
            "int main() {{ {} }}",
            gen_stmt(2)
        )
    }
}

#[test]
fn diff_expr() {
    let cases = [
        ("prec1", "int main(){return 1+2*3;}"),
        ("prec2", "int main(){return (1+2)*3;}"),
        ("assoc1", "int main(){return 10-3-2;}"),
        ("assoc2", "int main(){return 10-(3-2);}"),
        ("unary1", "int main(){return -1 + 2;}"),
        ("unary2", "int main(){return -(1 + 2);}"),
        ("mix1", "int main(){return 2*3+4*5;}"),
    ];

    for (name, src) in cases {
        diff_run(name, src);
    }
}

#[test]
fn diff_control() {
    let cases = [
        ("if1", "int main(){ if(1) return 1; else return 2; }"),
        ("if2", "int main(){ if(0) return 1; else return 2; }"),
        ("while1", "int main(){ int i=0; while(i<5) i=i+1; return i; }"),
        ("for1", "int main(){ int i; int s=0; for(i=0;i<5;i=i+1) s+=i; return s; }"),
    ];

    for (name, src) in cases {
        diff_run(name, src);
    }
}

#[test]
fn diff_vars() {
    let cases = [
        ("var1", "int main(){ int a=5; return a; }"),
        ("var2", "int main(){ int a=5; int b=6; return a+b; }"),
        ("scope1", "int main(){ int a=1; { int a=2; } return a; }"),
    ];

    for (name, src) in cases {
        diff_run(name, src);
    }
}

#[test]
fn diff_pointers() {
    let cases = [
        ("ptr1", "int main(){ int a=5; int* p=&a; return *p; }"),
        ("ptr2", "int main(){ int a=5; int* p=&a; *p=7; return a; }"),
    ];

    for (name, src) in cases {
        diff_run(name, src);
    }
}

// #[test]
// fn fuzz() {
//     for i in 0..1000 {
//         let src = util::gen_program();
//         diff_run(&format!("fuzz_{}", i), &src);
//     }
// }

#[test]
fn diff_arith() {
    let src = include_str!("arith.c");
    diff_run("arith", src);
}

#[test]
fn diff_struct_abi() {
    let src = include_str!("struct-abi.c");
    diff_run("struct_abi", src);
}

#[test]
fn diff_struct_abi2() {
    let src = include_str!("struct-abi2.c");
    diff_run("struct_abi2", src);
}
