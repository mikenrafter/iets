//! Import from derivation: the outputs a path's string context names, built
//! through the eval store before the evaluation reads them
//! (primops.cc:realiseContext). The upload and the build are the ones every
//! build takes: the plan of this evaluation's derivations and sources, then one
//! `BuildPathsWithResults`.

use crate::api::build::{locate_store, plan};
use crate::error::Result;
use crate::eval::Evaluator;
use crate::log;
use crate::remote::event::Event;
use crate::remote::worker::{Options, Transport};
use crate::store::path::StorePath;

use super::backend::Worker;
use super::upload::{PlanUpload, Uploader};

/// Why [`realise`] built nothing.
pub enum Refused {
    /// The whole message: the store's own for a build that failed, else why
    /// the build could not start.
    Message(String),
    /// A derivation to upload is not in the store, and this run dropped its
    /// ATerm text (`EvalConfig::keep_aterm`).
    DroppedText(String),
}

/// Builds each `(drv, output)` in the eval store and gives the output paths,
/// in order. The store must be a daemon on this machine: the evaluation reads
/// the outputs from `/nix/store` (docs/reference/divergences.md). A message
/// names `first` as Nix does (`drv^out`).
pub fn realise(
    ev: &Evaluator,
    wanted: &[(StorePath, String)],
    first: &str,
) -> std::result::Result<Vec<String>, Refused> {
    let cannot =
        |why: String| Refused::Message(format!("cannot build '{first}' during evaluation: {why}"));
    let store = ev
        .session()
        .nix_conf
        .get()
        .map_or("auto".into(), |c| c.store());
    let Some(t @ Transport::Unix(_)) = Transport::parse(&store) else {
        return Err(cannot(format!(
            "the store '{store}' is not a daemon on this machine, so its outputs cannot be read here"
        )));
    };
    // the daemon's own lines and the build's log, as nix-instantiate shows them
    let mut said = |e: Event| {
        if let Event::Log(l) = e {
            log::line(&String::from_utf8_lossy(&l.line));
        }
    };
    let mut roots: Vec<StorePath> = wanted.iter().map(|(d, _)| d.clone()).collect();
    roots.sort();
    roots.dedup();
    // the plan reads a derivation this run did not keep from `/nix/store`
    let plan = match plan(ev, &roots) {
        Ok(p) => p,
        Err(e) if !ev.drv.keeps_aterm() => return Err(Refused::DroppedText(e.to_string())),
        Err(e) => return Err(cannot(e.to_string())),
    };
    let up = |said: &mut dyn FnMut(Event)| -> Result<Worker> {
        let mut w = Worker::open(&t, &Options::default(), said)?;
        PlanUpload::new(&plan, &locate_store).upload(&mut w, &roots, said)?;
        Ok(w)
    };
    let mut w = up(&mut said).map_err(|e| cannot(format!("{e} (store '{store}')")))?;
    let targets: Vec<String> = wanted.iter().map(|(d, o)| format!("{d}!{o}")).collect();
    let results = (w.conn.build_paths_with_results(&targets, &mut said))
        .map_err(|e| cannot(e.to_string()))?;
    if let Some(r) = results.iter().find(|r| !r.success()) {
        return Err(Refused::Message(r.msg.clone()));
    }
    let mut out = Vec::with_capacity(wanted.len());
    for (drv, o) in wanted {
        let drv = drv.to_string();
        let map = (w.conn.query_derivation_output_map(&drv, &mut said))
            .map_err(|e| cannot(e.to_string()))?;
        match map.get(o).cloned().flatten() {
            Some(p) => out.push(p),
            None => return Err(cannot(format!("the store has no path for output '{o}'"))),
        }
    }
    Ok(out)
}
