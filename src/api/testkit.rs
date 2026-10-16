//! Evaluation in this process, for the unit tests that would otherwise
//! spawn `iets eval`.

use crate::api::run::Run;
use crate::api::{self, EvalArgs};
use crate::config::Config;
use crate::env::Env;
use crate::print;
use crate::session::Session;
use crate::sink::Sink;
use std::collections::HashSet;

/// One evaluation as `iets eval` does it: stdout, exit code and stderr.
pub fn eval(a: EvalArgs) -> (String, i32, String) {
    eval_with(a, &[])
}

/// [`eval`] with nix.conf settings, as `--option NAME VALUE` gives them.
pub fn eval_with(a: EvalArgs, options: &[(String, String)]) -> (String, i32, String) {
    let session = Session::with_env(Env::isolated());
    let mut conf = Config::empty();
    for (name, value) in options {
        conf.set(name, value);
    }
    let _ = session.nix_conf.set(conf);
    let log = Sink::default();
    session.log.write_to(Box::new(log.clone()));
    let _entered = session.enter();
    let strict = a.strict;
    let mut run = match Run::new(&a) {
        Ok(run) => run,
        Err(rc) => return (String::new(), rc, log.text()),
    };
    if a.dump_drvs.is_some() {
        run.cfg = run.cfg.keep_aterm(true).keep_site(false);
    }
    let (out, rc) = run
        .go(move |ev, _| {
            let r = api::evaluate(ev, a).and_then(|v| {
                let v = if strict {
                    ev.force_deep(v)?
                } else {
                    ev.force(v)?
                };
                let mut out = Vec::new();
                print::print_ambiguous(ev, v, 1, &mut out, &mut HashSet::new(), 0)?;
                Ok(out)
            });
            match r {
                Ok(out) => {
                    ev.lints.flush();
                    (out, 0)
                }
                Err(e) => (Vec::new(), api::report(ev, &e)),
            }
        })
        .expect("the fiber stacks");
    (String::from_utf8_lossy(&out).into_owned(), rc, log.text())
}
