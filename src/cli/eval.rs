//! `iets eval`: evaluate an expression and print it.

use std::collections::HashSet;
use std::time::Instant;

use crate::cli::{self, EvalArgs, evaluate, render, report, run_for};
use crate::eval::{Error, ErrorKind, Evaluator, Pos};
use crate::trace::run::Cache;
use crate::trace::store::Part;
use crate::{print, treestore};

pub fn run_eval(a: EvalArgs) -> i32 {
    let mut run = run_for(&a).cached(&a, "eval");
    if let Some(out) = run.hit(|c| {
        let out = c.part(Part::Result)?;
        c.replay_warnings();
        Some(out)
    }) {
        render::bytes(&out);
        return 0;
    }
    // only the cache and --dump-drvs read the ATerm text after evaluation,
    // and an import from derivation that uploads a derivation: a run that
    // dropped one evaluates again keeping them (docs/reference/divergences.md)
    let mut keep = run.cache.is_some() || a.dump_drvs.is_some();
    loop {
        run.cfg = run.cfg.keep_aterm(keep).keep_site(false);
        let args = a.clone();
        // parallel: the rest runs as the main fiber
        let again = !keep;
        if let Some(code) =
            cli::run_parallel(run, move |ev, cache| eval_and_print(ev, args, cache, again))
        {
            return code;
        }
        keep = true;
        run = run_for(&a).cached(&a, "eval");
    }
}

/// The exit code, or `None` when `again` and an import from derivation needs
/// a derivation text the run dropped.
fn eval_and_print(
    ev: &mut Evaluator,
    a: EvalArgs,
    cache: Option<Cache>,
    again: bool,
) -> Option<i32> {
    let strict = a.strict;
    let json = a.json;
    let raw = a.raw;
    let t0 = Instant::now();
    let timings = a.timings;
    let evaluating = cli::evaluating(&a);
    let dropped = |e: &Error| again && e.kind == ErrorKind::DroppedText;
    let v = match evaluate(ev, a) {
        Ok(v) => v,
        Err(e) if dropped(&e) => return None,
        Err(e) => return Some(report(ev, &e)),
    };

    let mut out: Vec<u8> = Vec::new();
    let r = if raw {
        // nix eval --raw: coerce without copying to the store, no newline
        let mut ctx = Vec::new();
        ev.coerce_to_string(
            v,
            &mut ctx,
            Pos::NONE,
            "while generating the eval command output",
            false,
            false,
            true,
        )
        .map(|s| out.extend_from_slice(&s))
    } else if json {
        let mut ctx = Vec::new();
        print::print_json(ev, v, &mut out, &mut ctx, strict, Pos::NONE)
    } else {
        (|| {
            let v = if strict {
                ev.force_deep(v)?
            } else {
                ev.force(v)?
            };
            let mut seen = HashSet::new();
            print::print_ambiguous(ev, v, 1, &mut out, &mut seen, 0)
        })()
    };
    drop(evaluating);
    if ev.session().settings().stats >= 1 {
        ev.print_stats();
    }
    match r {
        Ok(()) => {
            if !raw {
                out.push(b'\n');
            }
            render::bytes(&out);
            if timings {
                eprintln!("eval: {:.3}s", t0.elapsed().as_secs_f64());
                treestore::report_timings();
            }
            if let Some(c) = &cache {
                c.write(ev, &out, false);
            }
            Some(0)
        }
        Err(e) if dropped(&e) => None,
        Err(e) => Some(report(ev, &e)),
    }
}
