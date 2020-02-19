// MIT License
//
// Copyright (c) 2019 Alasdair Armstrong
//
// Permission is hereby granted, free of charge, to any person
// obtaining a copy of this software and associated documentation
// files (the "Software"), to deal in the Software without
// restriction, including without limitation the rights to use, copy,
// modify, merge, publish, distribute, sublicense, and/or sell copies
// of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be
// included in all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
// EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
// MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
// NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS
// BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN
// ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN
// CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

use crossbeam::queue::SegQueue;
use serde::de::DeserializeOwned;
use std::collections::{HashMap, HashSet};
use std::convert::TryFrom;
use std::env;
use std::error::Error;
use std::fs;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::process;
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;

use isla_cat::cat;
use isla_cat::smt::compile_cat;

use isla_lib::axiomatic::run_litmus;
use isla_lib::axiomatic::ExecutionInfo;
use isla_lib::axiomatic::model::Model;
use isla_lib::concrete::{B64, BV};
use isla_lib::config::ISAConfig;
use isla_lib::footprint_analysis::footprint_analysis;
use isla_lib::init::{initialize_architecture, Initialized};
use isla_lib::ir::serialize as ir_serialize;
use isla_lib::ir::*;
use isla_lib::litmus::Litmus;
use isla_lib::memory::Memory;
use isla_lib::simplify::{write_events_with_opts, WriteOpts};
use isla_lib::smt::smtlib;
use isla_lib::smt::Event;

use getopts::Options;
mod request;
use request::{Request, Response};

mod smt_events;
use smt_events::smt_of_candidate;

static THREADS: usize = 4;
static LIMIT_MEM_BYTES: u64 = 2048 * 1024 * 1024;
static LIMIT_CPU_SECONDS: u64 = 60;

fn setrlimit(resource: libc::__rlimit_resource_t, soft: u64, hard: u64) -> std::io::Result<()> {
    assert!(std::mem::size_of::<libc::rlim_t>() == 8);

    let ret = unsafe {
        libc::setrlimit(resource, &libc::rlimit { rlim_cur: soft as libc::rlim_t, rlim_max: hard as libc::rlim_t })
    };

    if ret == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn limit() -> std::io::Result<()> {
    setrlimit(libc::RLIMIT_AS, LIMIT_MEM_BYTES, LIMIT_MEM_BYTES)?;
    setrlimit(libc::RLIMIT_CPU, LIMIT_CPU_SECONDS, LIMIT_CPU_SECONDS)
}

/// Main just sets resource limits then calls handle_request to do the
/// actual work.
fn main() {
    if let Err(_) = limit() {
        eprintln!("Failed to set resource limits");
        std::process::exit(1)
    }

    let response = match handle_request() {
        Ok(resp) => match serde_json::to_vec(&resp) {
            Ok(resp) => resp,
            Err(e) => {
                eprintln!("{}", e);
                std::process::exit(1)
            }
        },
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(1)
        }
    };

    unsafe { isla_lib::smt::finalize_solver() };

    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    handle.write_all(&response).unwrap()
}

fn deserialize_from_stdin<T: DeserializeOwned>() -> Option<T> {
    let stdin = std::io::stdin();
    let handle = stdin.lock();
    bincode::deserialize_from(handle).ok()
}

static ARCH_WHITELIST: [&str; 1] = ["aarch64"];

/// The error handling scheme is as follows. If we have an expected
/// error condition (i.e. a flaw in the user input), then that is
/// returned normally as part of the response using
/// Response::Error. This function either panics or returns Err on
/// unexpected errors.
fn handle_request() -> Result<Response, Box<dyn Error>> {
    let args: Vec<String> = std::env::args().collect();
    let mut opts = Options::new();
    opts.reqopt("", "resources", "path to resource files", "<path>");
    opts.reqopt("", "cache", "path to a cache directory", "<path>");

    let matches = opts.parse(&args[1..])?;

    // Log absolutely everything
    isla_lib::log::set_flags(0xffff_ffff);

    // The main server process sends the json from the AJAX request to
    // us via stdin. It should never be invalid.
    let req = deserialize_from_stdin::<Request>().expect("Invalid arguments");

    // Check that the request architecture is valid
    if !ARCH_WHITELIST.contains(&req.arch.as_str()) {
        panic!("Invalid architecture in request");
    }

    let resources = PathBuf::from(matches.opt_str("resources").unwrap());
    if !resources.is_dir() {
        panic!("Invalid resources directory");
    }

    let cache = PathBuf::from(matches.opt_str("cache").unwrap());
    if !cache.is_dir() {
        panic!("Invalid cache directory");
    }

    let now = Instant::now();

    let config_file = resources.join(format!("{}.toml", req.arch));
    let symtab_file = resources.join(format!("{}.symtab", req.arch));
    let ir_file = resources.join(format!("{}.irx", req.arch));

    let strings: Vec<String> = bincode::deserialize(&fs::read(&symtab_file)?)?;
    let symtab = Symtab::from_raw_table(&strings);

    let mut ir: Vec<Def<u32, B64>> = ir_serialize::deserialize(&fs::read(&ir_file)?).expect("Failed to deserialize IR");

    let isa_config: ISAConfig<B64> = ISAConfig::parse(&fs::read_to_string(&config_file)?, &symtab)?;

    eprintln!("Loaded architecture in: {}ms", now.elapsed().as_millis());

    let litmus = match Litmus::parse(&req.litmus, &symtab, &isa_config) {
        Ok(litmus) => litmus,
        Err(e) => return Ok(Response::Error { message: format!("Failed to process litmus file:\n{}\n", e) }),
    };
    litmus.log();

    let now = Instant::now();

    let parse_cat = match cat::ParseCat::from_string(&req.cat) {
        Ok(parse_cat) => parse_cat,
        Err(e) => {
            return Ok(Response::Error { message: format!("Parse error in cat: {}\n", e) });
        }
    };

    let cat = match cat::resolve_includes(&[], parse_cat) {
        Ok(cat) => {
            let mut tcx = cat::initial_tcx(isa_config.fences.iter().map(String::clone));
            match cat::infer_cat(&mut tcx, cat) {
                Ok(cat) => cat,
                Err(e) => {
                    return Ok(Response::Error { message: format!("Type error in cat: {:?}\n", e) });
                }
            }
        }
        Err(e) => {
            return Ok(Response::Error { message: format!("Error in cat file: {}\n", e) });
        }
    };

    eprintln!("Parsed user input in: {}us", now.elapsed().as_micros());

    let Initialized { regs, lets, shared_state } =
        initialize_architecture(&mut ir, symtab, &isa_config, AssertionMode::Optimistic);

    let graph_queue = SegQueue::new();

    let run_info = run_litmus::litmus_per_candidate(
        THREADS,
        &litmus,
        regs,
        lets,
        &shared_state,
        &isa_config,
        &cache,
        &|tid, candidate, footprints| {
            let now = Instant::now();

            let exec = ExecutionInfo::from(&candidate).unwrap();

            let mut path = env::temp_dir();
            path.push(format!("isla_candidate_{}_{}.smt2", process::id(), tid));

            // Create the SMT file with all the thread traces and the cat model.
            {
                let mut fd = File::create(&path).unwrap();
                writeln!(&mut fd, "(set-option :produce-models true)");

                let mut enums = HashSet::new();
                for thread in candidate {
                    for event in *thread {
                        if let Event::Smt(smtlib::Def::DefineEnum(_, size)) = event {
                            enums.insert(*size);
                        }
                    }
                }

                for size in enums {
                    write!(&mut fd, "(declare-datatypes ((Enum{} 0)) ((", size).unwrap();
                    for i in 0..size {
                        write!(&mut fd, "(e{}_{})", size, i).unwrap()
                    }
                    writeln!(&mut fd, ")))").unwrap()
                }

                for thread in candidate {
                    write_events_with_opts(&mut fd, thread, &shared_state.symtab, &WriteOpts::smtlib()).unwrap()
                }

                smt_of_candidate(&mut fd, &exec, &litmus, footprints, &shared_state, &isa_config);

                compile_cat(&mut fd, &cat);

                writeln!(&mut fd, "(check-sat)");
                writeln!(&mut fd, "(get-model)");
            }

            let z3 = Command::new("z3").arg(&path).output().expect("Failed to execute z3");

            let z3_output = std::str::from_utf8(&z3.stdout).expect("z3 output was not utf-8 encoded");

            if z3_output.starts_with("sat") {
                let mut event_names: Vec<&str> = exec.events.iter().map(|ev| ev.name.as_ref()).collect();
                event_names.push("IW");
                let model_buf = &z3_output[3..];
                let mut model = Model::<B64>::parse(&event_names, model_buf).expect("Failed to parse model");

                eprintln!("{}", model_buf);
                eprintln!("co: {:#?}", model.interpret_rel("co", &event_names));
                eprintln!("rf: {:#?}", model.interpret_rel("rf", &event_names));
                eprintln!("rfi: {:#?}", model.interpret_rel("rfi", &event_names));
                eprintln!("rfe: {:#?}", model.interpret_rel("rfe", &event_names));
                eprintln!("fre: {:#?}", model.interpret_rel("fre", &event_names));
                eprintln!("coi: {:#?}", model.interpret_rel("coi", &event_names));
                eprintln!("coe: {:#?}", model.interpret_rel("coe", &event_names));
                eprintln!("obs: {:#?}", model.interpret_rel("obs", &event_names));
                eprintln!("dob: {:#?}", model.interpret_rel("dob", &event_names));
                eprintln!("aob: {:#?}", model.interpret_rel("aob", &event_names));
                eprintln!("bob: {:#?}", model.interpret_rel("bob", &event_names));
                eprintln!("ob: {:#?}", model.interpret_rel("ob", &event_names));

                let mut model_path = env::temp_dir();
                model_path.push(format!("isla_candidate_{}_{}.model", process::id(), tid));
                fs::write(&model_path, z3_output);

                let isla_viz = Command::new("isla-viz")
                    .arg("--input")
                    .arg(&model_path)
                    .arg("IW")
                    .args(exec.events.iter().map(|ev| &ev.name).collect::<Vec<_>>())
                    .output()
                    .expect("Failed to execute isla-viz");

                let isla_viz_output =
                    String::from_utf8(isla_viz.stdout).expect("isla_viz output was not utf-8 encoded");

                graph_queue.push(isla_viz_output);

                eprintln!("sat in: {}ms", now.elapsed().as_millis());
            } else if z3_output.starts_with("unsat") {
                eprintln!("unsat in: {}ms", now.elapsed().as_millis())
            } else {
                eprintln!("z3 error")
            }
        },
    )
    .unwrap();

    let mut graphs: Vec<String> = Vec::new();
    loop {
        match graph_queue.pop() {
            Ok(graph) => graphs.push(graph),
            Err(_) => break,
        }
    }

    Ok(Response::Done {
        graphs: vec![],
        objdump: litmus.objdump,
        candidates: i32::try_from(run_info.candidates).expect("Candidates did not fit in i32"),
    })
}
