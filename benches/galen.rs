//! Galen benchmark from
//! `https://github.com/frankmcsherry/dynamic-datalog/tree/master/problems/galen`

use csv::{Reader as CsvReader, ReaderBuilder};
use dbsp::{
    algebra::HasZero,
    circuit::{trace::SchedulerEvent, GlobalNodeId, Root, Runtime, Stream},
    monitor::TraceMonitor,
    operator::{CsvSource, DelayedFeedback},
    profile::CPUProfiler,
    trace::{
        ord::{OrdIndexedZSet, OrdZSet},
        BatchReader,
    },
};
use std::{collections::HashMap, fmt::Write, fs, fs::File, path::PathBuf};

/*
.decl p(X: Number, Z: Number)
.decl q(X: Number, Y: Number, Z: Number)
.decl r(R: Number, P: Number, E: Number)
.decl c(Y: Number, Z: Number, W: Number)
.decl u(R: Number, Z: Number, W: Number)
.decl s(R: Number, P: Number)

.input p(IO="file", filename="p.txt", delimiter=",")
.input q(IO="file", filename="q.txt", delimiter=",")
.input r(IO="file", filename="r.txt", delimiter=",")
.input c(IO="file", filename="c.txt", delimiter=",")
.input u(IO="file", filename="u.txt", delimiter=",")
.input s(IO="file", filename="s.txt", delimiter=",")

p(?x,?z) :- p(?x,?y), p(?y,?z).
p(?x,?z) :- p(?y,?w), u(?w,?r,?z), q(?x,?r,?y).
p(?x,?z) :- c(?y,?w,?z),p(?x,?w), p(?x,?y).
q(?x,?r,?z) :- p(?x,?y), q(?y,?r,?z).
q(?x,?q,?z) :- q(?x,?r,?z),s(?r,?q).
q(?x,?e,?o) :- q(?x,?y,?z),r(?y,?u,?e),q(?z,?u,?o).
*/

type Number = u32;
type Weight = isize;

fn csv_source<T>(file: &str) -> CsvSource<File, T, Weight, OrdZSet<T, Weight>>
where
    T: Clone + Ord,
{
    let path: PathBuf = ["benches", "galen_data", file].iter().collect();

    let reader: CsvReader<File> = ReaderBuilder::new()
        .delimiter(b',')
        .has_headers(false)
        .from_path(path)
        .unwrap();
    CsvSource::from_csv_reader(reader)
}

fn main() {
    let hruntime = Runtime::run(1, |_runtime, _index| {
        let monitor = TraceMonitor::new_panic_on_error();

        let root = Root::build(|circuit| {
            /*
            let cpu_profiler = CPUProfiler::new();
            cpu_profiler.attach(circuit, "cpu profiler");

            monitor.attach(circuit, "monitor");

            let mut metadata = <HashMap<GlobalNodeId, String>>::new();
            let mut nsteps = 0;
            let monitor_clone = monitor.clone();
            circuit.register_scheduler_event_handler("metadata", move |event: &SchedulerEvent| {
                match event {
                    SchedulerEvent::EvalEnd { node } => {
                        let metadata_string = metadata
                            .entry(node.global_id().clone())
                            .or_insert_with(|| String::new());
                        metadata_string.clear();
                        node.summary(metadata_string);
                    }
                    SchedulerEvent::StepEnd => {
                        let graph = monitor_clone.visualize_circuit_annotate(&|node_id| {
                            let mut metadata_string = metadata
                                .get(node_id)
                                .map(ToString::to_string)
                                .unwrap_or_else(|| "".to_string());
                            writeln!(metadata_string, "id: {}", node_id).unwrap();
                            if let Some(cpu_profile) = cpu_profiler.operator_profile(node_id) {
                                writeln!(
                                    metadata_string,
                                    "invocations: {}",
                                    cpu_profile.invocations()
                                )
                                .unwrap();
                                writeln!(metadata_string, "time: {:?}", cpu_profile.total_time())
                                    .unwrap();
                            };
                            metadata_string
                        });
                        fs::write(format!("galen.{}.dot", nsteps), graph.to_dot()).unwrap();
                        nsteps += 1;
                    }
                    _ => {}
                }
            });
            */

            let p_source = csv_source::<(Number, Number)>("p.txt");
            let q_source = csv_source::<(Number, Number, Number)>("q.txt");
            let r_source = csv_source::<(Number, Number, Number)>("r.txt");
            let c_source = csv_source::<(Number, Number, Number)>("c.txt");
            let u_source = csv_source::<(Number, Number, Number)>("u.txt");
            let s_source = csv_source::<(Number, Number)>("s.txt");

            let p: Stream<_, OrdZSet<_, Weight>> =
                circuit.region("p", || circuit.add_source(p_source));
            let q: Stream<_, OrdZSet<_, Weight>> =
                circuit.region("q", || circuit.add_source(q_source));
            let r: Stream<_, OrdZSet<_, Weight>> =
                circuit.region("r", || circuit.add_source(r_source));
            let c: Stream<_, OrdZSet<_, Weight>> =
                circuit.region("c", || circuit.add_source(c_source));
            let u: Stream<_, OrdZSet<_, Weight>> =
                circuit.region("u", || circuit.add_source(u_source));
            let s: Stream<_, OrdZSet<_, Weight>> =
                circuit.region("s", || circuit.add_source(s_source));

            let (outp, outq) = circuit
                .fixedpoint(|child| {
                    let pvar: DelayedFeedback<_, OrdZSet<(Number, Number), Weight>> =
                        DelayedFeedback::new(child);
                    let qvar: DelayedFeedback<_, OrdZSet<(Number, Number, Number), Weight>> =
                        DelayedFeedback::new(child);

                    let p_by_1 = pvar.stream().index::<OrdIndexedZSet<_, _, _>>();
                    let p_by_2 = pvar
                        .stream()
                        .index_with::<OrdIndexedZSet<_, _, _>, _>(|&(x, y)| (y, x));
                    let p_by_12 = pvar
                        .stream()
                        .index_with::<OrdIndexedZSet<_, _, _>, _>(|&(x, y)| ((x, y), ()));
                    let u_by_1 = u
                        .delta0(child)
                        .index_with::<OrdIndexedZSet<_, _, _>, _>(|&(x, y, z)| (x, (y, z)));
                    let q_by_1 = qvar
                        .stream()
                        .index_with::<OrdIndexedZSet<_, _, _>, _>(|&(x, y, z)| (x, (y, z)));
                    let q_by_2 = qvar
                        .stream()
                        .index_with::<OrdIndexedZSet<_, _, _>, _>(|&(x, y, z)| (y, (x, z)));
                    let q_by_12 = qvar
                        .stream()
                        .index_with::<OrdIndexedZSet<_, _, _>, _>(|&(x, y, z)| ((x, y), z));
                    let q_by_23 = qvar
                        .stream()
                        .index_with::<OrdIndexedZSet<_, _, _>, _>(|&(x, y, z)| ((y, z), x));
                    let c_by_2 = c
                        .delta0(child)
                        .index_with::<OrdIndexedZSet<_, _, _>, _>(|&(x, y, z)| (y, (x, z)));
                    let r_by_1 = r
                        .delta0(child)
                        .index_with::<OrdIndexedZSet<_, _, _>, _>(|&(x, y, z)| (x, (y, z)));
                    let s_by_1 = s.delta0(child).index::<OrdIndexedZSet<_, _, _>>();

                    // IR1: p(x,z) :- p(x,y), p(y,z).
                    let ir1 =
                        child.region("IR1", || p_by_2.join_trace(&p_by_1, |&_y, &x, &z| (x, z)));
                    ir1.inspect(|zs: &OrdZSet<_, _>| println!("ir1: {}", zs.len()));

                    // IR2: q(x,r,z) := p(x,y), q(y,r,z)
                    let ir2 = child.region("IR2", || {
                        p_by_2.join_trace(&q_by_1, |&_y, &x, &(r, z)| (x, r, z))
                    });

                    ir2.inspect(|zs: &OrdZSet<_, _>| println!("ir2: {}", zs.len()));

                    // IR3: p(x,z) := p(y,w), u(w,r,z), q(x,r,y)
                    let ir3 = child.region("IR3", || {
                        p_by_2
                            .join_trace::<_, _, OrdZSet<_, _>>(&u_by_1, |&_w, &y, &(r, z)| {
                                ((r, y), z)
                            })
                            .index::<OrdIndexedZSet<_, _, _>>()
                            .join_trace(&q_by_23, |&(_r, _y), &z, &x| (x, z))
                    });
                    ir3.inspect(|zs: &OrdZSet<_, _>| println!("ir3: {}", zs.len()));

                    // IR4: p(x,z) := c(y,w,z), p(x,w), p(x,y)
                    let ir4_1 = child.region("IR4-1", || {
                        c_by_2.join_trace::<_, _, OrdZSet<_, _>>(&p_by_2, |&_w, &(y, z), &x| {
                            ((x, y), z)
                        })
                    });
                    ir4_1.inspect(|zs: &OrdZSet<_, _>| println!("ir4_1: {}", zs.len()));

                    let ir4 = child.region("IR4-2", || {
                        ir4_1
                            .index::<OrdIndexedZSet<_, _, _>>()
                            .join_trace(&p_by_12, |&(x, _y), &z, &()| (x, z))
                    });
                    ir4.inspect(|zs: &OrdZSet<_, _>| println!("ir4: {}", zs.len()));

                    // IR5: q(x,q,z) := q(x,r,z), s(r,q)
                    let ir5 = child.region("IR5", || {
                        q_by_2.join_trace(&s_by_1, |&_r, &(x, z), &q| (x, q, z))
                    });
                    ir5.inspect(|zs: &OrdZSet<_, _>| println!("ir5: {}", zs.len()));

                    // IR6: q(x,e,o) := q(x,y,z), r(y,u,e), q(z,u,o)
                    let ir6_1 = child.region("IR6_1", || {
                        q_by_2
                            .join_trace::<_, _, OrdZSet<_, _>>(&r_by_1, |&_y, &(x, z), &(u, e)| {
                                ((z, u), (x, e))
                            })
                            .index::<OrdIndexedZSet<_, _, _>>()
                    });
                    let ir6 = child.region("IR6", || {
                        ir6_1.join_trace(&q_by_12, |&(_z, _u), &(x, e), &o| (x, e, o))
                    });

                    ir6.inspect(|zs: &OrdZSet<_, _>| println!("ir6: {}", zs.len()));

                    let p = p.delta0(child).sum([&ir1, &ir3, &ir4]).distinct_trace();

                    let q = q.delta0(child).sum([&ir2, &ir5, &ir6]).distinct_trace();

                    pvar.connect(&p);
                    qvar.connect(&q);

                    Ok((p.integrate_trace().export(), q.integrate_trace().export()))
                })
                .unwrap();
            outp.consolidate::<OrdZSet<_, _>>()
                .inspect(|zs: &OrdZSet<_, _>| println!("outp: {}", zs.len()));
            outq.consolidate::<OrdZSet<_, _>>()
                .inspect(|zs: &OrdZSet<_, _>| println!("outq: {}", zs.len()));
        })
        .unwrap();

        let graph = monitor.visualize_circuit();
        fs::write("galen.dot", graph.to_dot()).unwrap();

        root.step().unwrap();
    });

    hruntime.join().unwrap();
}
