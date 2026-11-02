//! hydo-grpc-gen: gRPC drivers for a hydo project (see the crate documentation).

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let tools = hydo_config::Config::process().tools;
    std::process::exit(hydo_grpc_gen::main_with(&argv, &tools));
}
