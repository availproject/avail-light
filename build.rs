const PROTOS: &[&str] = &[
    "src/network/schema/api.v1.proto",
    "src/network/schema/finality.v1.proto",
    "src/network/schema/light.v1.proto",
];

fn main() {
    prost_build::compile_protos(PROTOS, &["src/network/schema"]).unwrap();
}
