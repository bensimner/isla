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
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::env;
use std::fs::File;
use std::io::Write;
use std::process;
use std::process::exit;
use std::sync::Arc;
use std::time::Instant;

use isla_cat::cat;

use isla_lib::concrete::{B64, BV};
use isla_lib::executor;
use isla_lib::executor::LocalFrame;
use isla_lib::footprint_analysis::footprint_analysis;
use isla_lib::init::{initialize_architecture, Initialized};
use isla_lib::ir::*;
use isla_lib::litmus::Litmus;
use isla_lib::log;
use isla_lib::memory::Memory;
use isla_lib::smt::Event;

mod opts;
mod smt_events;

use opts::CommonOpts;
use smt_events::{smt_candidate, Candidates};

fn main() {
    let code = isla_main();
    unsafe { isla_lib::smt::finalize_solver() };
    exit(code)
}

fn isla_main() -> i32 {
    let mut opts = opts::common_opts();
    opts.reqopt("l", "litmus", "load a litmus file", "<file>");
    opts.reqopt("m", "model", "load a cat memory model", "<file>");
    opts.optopt("", "cache", "cache directory", "<path>");

    let now = Instant::now();
    let mut hasher = Sha256::new();
    let (matches, arch) = opts::parse(&mut hasher, &opts);
    let CommonOpts { num_threads, mut arch, symtab, isa_config } =
        opts::parse_with_arch(&mut hasher, &opts, &matches, &arch);

    let arch_hash = hasher.result();
    log!(log::VERBOSE, &format!("Archictecture + config hash: {:x}", arch_hash));
    log!(log::VERBOSE, &format!("Parsing took: {}ms", now.elapsed().as_millis()));

    let Initialized { regs, mut lets, shared_state } =
        initialize_architecture(&mut arch, symtab, &isa_config, AssertionMode::Optimistic);

    let litmus = match Litmus::from_file(matches.opt_str("litmus").unwrap(), &shared_state.symtab, &isa_config) {
        Ok(litmus) => litmus,
        Err(e) => {
            eprintln!("{}", e);
            return 1;
        }
    };

    let cat = match cat::load_cat(&matches.opt_str("model").unwrap()) {
        Ok(cat) => {
            let mut tcx = cat::initial_tcx(isa_config.barriers.values().map(String::clone));
            match cat::infer_cat(&mut tcx, cat) {
                Ok(cat) => cat,
                Err(e) => {
                    eprintln!("Type error in cat: {:?}", e);
                    return 1;
                }
            }
        }
        Err(e) => {
            eprintln!("Could not load cat: {}", e);
            return 1;
        }
    };

    /*
    {
        let stdout = io::stdout();
        let mut handle = stdout.lock();
        isla_cat::smt::compile_cat(&mut handle, &cat).expect("Failed to compile cat");
    }
    */

    let mut memory = Memory::new();
    memory.add_concrete_region(isa_config.thread_base..isa_config.thread_top, HashMap::new());

    let mut current_base = isa_config.thread_base;
    for (thread, _, code) in litmus.assembled.iter() {
        log!(log::VERBOSE, &format!("Thread {} @ 0x{:x}", thread, current_base));
        for (i, byte) in code.iter().enumerate() {
            memory.write_byte(current_base + i as u64, *byte)
        }
        current_base += isa_config.thread_stride
    }

    litmus.log();
    memory.log();

    let function_id = shared_state.symtab.lookup("zmain");
    let (args, _, instrs) = shared_state.functions.get(&function_id).unwrap();
    let tasks: Vec<_> = litmus
        .assembled
        .iter()
        .enumerate()
        .map(|(i, (_, inits, _))| {
            let address = isa_config.thread_base + (isa_config.thread_stride * i as u64);
            lets.insert(ELF_ENTRY, UVal::Init(Val::I128(address as i128)));
            let mut regs = regs.clone();
            for (reg, value) in inits {
                regs.insert(*reg, UVal::Init(Val::Bits(B64::from_u64(*value))));
            }
            LocalFrame::new(args, Some(&[Val::Unit]), instrs)
                .add_lets(&lets)
                .add_regs(&regs)
                .set_memory(memory.clone())
                .task(i)
        })
        .collect();

    let mut thread_buckets: Vec<Vec<Vec<Event<B64>>>> = vec![Vec::new(); tasks.len()];
    let queue = Arc::new(SegQueue::new());

    let now = Instant::now();
    executor::start_multi(num_threads, tasks, &shared_state, queue.clone(), &executor::trace_collector);
    log!(log::VERBOSE, &format!("Symbolic execution took: {}ms", now.elapsed().as_millis()));

    let rk_ifetch = match shared_state.enum_member("Read_ifetch") {
        Some(rk) => rk,
        None => {
            eprintln!("No `Read_ifetch' read kind found in specified architecture!");
            return 1;
        }
    };

    loop {
        match queue.pop() {
            Ok(Ok((task_id, mut events))) => {
                let events: Vec<Event<B64>> = events
                    .drain(..)
                    .rev()
                    .filter(|ev| {
                        (ev.is_memory() && !ev.has_read_kind(rk_ifetch))
                            || ev.is_smt()
                            || ev.is_instr()
                            || ev.is_cycle()
                            || ev.is_write_reg()
                    })
                    .collect();
                let mut events = isla_lib::simplify::remove_unused(events.to_vec());
                for event in events.iter_mut() {
                    isla_lib::simplify::renumber_event(event, task_id as u32, thread_buckets.len() as u32)
                }

                thread_buckets[task_id].push(events)
            }
            // Error during execution
            Ok(Err(msg)) => {
                eprintln!("{}", msg);
                return 1;
            }
            // Empty queue
            Err(_) => break,
        }
    }

    let footprints =
        footprint_analysis(num_threads, &thread_buckets, &lets, &regs, &shared_state, &isa_config, "cache").unwrap();

    let candidates = Candidates::new(&thread_buckets);

    log!(log::VERBOSE, &format!("There are {} candidate executions", candidates.total()));

    for (i, candidate) in candidates.enumerate() {
        let mut path = env::temp_dir();
        path.push(format!("isla_candidate_{}_{}", process::id(), i));
        let mut fd = File::create(path).unwrap();

        smt_candidate(&mut fd, &candidate, &litmus, &footprints, &shared_state)
            .expect("Failed to generate candidate execution");
        isla_cat::smt::compile_cat(&mut fd, &cat).expect("Failed to compile cat");
        writeln!(&mut fd, "(check-sat)").unwrap();
    }

    0
}
