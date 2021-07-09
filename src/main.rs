use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;

use ethabi::Contract;
use graph::data::subgraph::*;
use graph::{
    blockchain::BlockPtr,
    components::store::DeploymentLocator,
    data::subgraph::{Mapping, Source, TemplateSource},
    ipfs_client::IpfsClient,
    prelude::{
        o, slog, BlockState, DeploymentHash, HostMetrics, Link, Logger, StopwatchMetrics,
        SubgraphStore,
    },
    semver::Version,
};
use graph_chain_arweave::adapter::ArweaveAdapter;
use graph_chain_ethereum::{Chain, DataSource, DataSourceTemplate};
use graph_core::three_box::ThreeBoxAdapter;
use graph_mock::MockMetricsRegistry;
use graph_runtime_wasm::mapping::ValidModule;
use graph_runtime_wasm::{
    host_exports::HostExports, mapping::MappingContext, module::ExperimentalFeatures,
};
use slog::*;
use test_store::STORE;
use web3::types::Address;

use custom_wasm_instance::WasmInstance;

mod custom_wasm_instance;

fn mock_host_exports(
    subgraph_id: DeploymentHash,
    data_source: DataSource,
    store: Arc<impl SubgraphStore>,
) -> HostExports<Chain> {
    let arweave_adapter = Arc::new(ArweaveAdapter::new("https://arweave.net".to_string()));
    let three_box_adapter = Arc::new(ThreeBoxAdapter::new("https://ipfs.3box.io/".to_string()));

    let templates = vec![DataSourceTemplate {
        kind: String::from("ethereum/contract"),
        name: String::from("example template"),
        network: Some(String::from("mainnet")),
        source: TemplateSource {
            abi: String::from("foo"),
        },
        mapping: Mapping {
            kind: String::from("ethereum/events"),
            api_version: Version::parse("0.1.0").expect("Could not parse api version."),
            language: String::from("wasm/assemblyscript"),
            entities: vec![],
            abis: vec![],
            event_handlers: vec![],
            call_handlers: vec![],
            block_handlers: vec![],
            link: Link {
                link: "link".to_owned(),
            },
            runtime: Arc::new(vec![]),
        },
    }];

    let network = data_source.network.clone().expect("Could not get network.");
    HostExports::new(
        subgraph_id,
        &data_source,
        network,
        Arc::new(templates),
        Arc::new(graph_core::LinkResolver::from(IpfsClient::localhost())),
        store,
        arweave_adapter,
        three_box_adapter,
    )
}

fn mock_context(
    deployment: DeploymentLocator,
    data_source: DataSource,
    store: Arc<impl SubgraphStore>,
) -> MappingContext<Chain> {
    MappingContext {
        logger: test_store::LOGGER.clone(),
        block_ptr: BlockPtr {
            hash: Default::default(),
            number: 0,
        },
        host_exports: Arc::new(mock_host_exports(
            deployment.hash.clone(),
            data_source,
            store.clone(),
        )),
        state: BlockState::new(
            store
                .writable(&deployment)
                .expect("Could not create BlockState."),
            Default::default(),
        ),
        proof_of_indexing: None,
        host_fns: Arc::new(Vec::new()),
    }
}

fn mock_abi() -> MappingABI {
    MappingABI {
        name: "mock_abi".to_string(),
        contract: Contract::load(
            r#"[
            {
                "inputs": [
                    {
                        "name": "a",
                        "type": "address"
                    }
                ],
                "type": "constructor"
            }
        ]"#
            .as_bytes(),
        )
        .expect("Could not load contract."),
    }
}

fn mock_data_source(path: &str) -> DataSource {
    let runtime = std::fs::read(path).expect("Could not resolve path to wasm file.");

    DataSource {
        kind: String::from("ethereum/contract"),
        name: String::from("example data source"),
        network: Some(String::from("mainnet")),
        source: Source {
            address: Some(
                Address::from_str("0123123123012312312301231231230123123123")
                    .expect("Could not create address from string."),
            ),
            abi: String::from("123123"),
            start_block: 0,
        },
        mapping: Mapping {
            kind: String::from("ethereum/events"),
            api_version: Version::parse("0.1.0").expect("Could not parse api version."),
            language: String::from("wasm/assemblyscript"),
            entities: vec![],
            abis: vec![],
            event_handlers: vec![],
            call_handlers: vec![],
            block_handlers: vec![],
            link: Link {
                link: "link".to_owned(),
            },
            runtime: Arc::new(runtime),
        },
        context: Default::default(),
        creation_block: None,
        contract_abi: Arc::new(mock_abi()),
    }
}

pub fn main() {
    let plain = slog_term::PlainSyncDecorator::new(std::io::stdout());
    let logger = Logger::root(slog_term::FullFormat::new(plain).build().fuse(), o!());
    let now = Instant::now();
    let args: Vec<String> = std::env::args().collect();

    if args.len() == 1 {
        panic!("Must provide path to wasm file.")
    }

    let path_to_wasm = &args[1];

    let subgraph_id = "ipfsMap";
    let deployment_id = DeploymentHash::new(subgraph_id).expect("Could not create DeploymentHash.");

    let deployment = test_store::create_test_subgraph(
        &deployment_id,
        "type User @entity {
            id: ID!,
            name: String,
        }
    
        type Thing @entity {
            id: ID!,
            value: String,
            extra: String
        }",
    );

    let data_source = mock_data_source(path_to_wasm);

    let store = STORE.clone();

    let metrics_registry = Arc::new(MockMetricsRegistry::new());

    let stopwatch_metrics = StopwatchMetrics::new(
        Logger::root(slog::Discard, o!()),
        deployment_id.clone(),
        metrics_registry.clone(),
    );

    let host_metrics = Arc::new(HostMetrics::new(
        metrics_registry,
        deployment_id.as_str(),
        stopwatch_metrics,
    ));

    let experimental_features = ExperimentalFeatures {
        allow_non_deterministic_ipfs: true,
        allow_non_deterministic_arweave: true,
        allow_non_deterministic_3box: true,
    };

    let valid_module = Arc::new(
        ValidModule::new(data_source.mapping.runtime.as_ref())
            .expect("Could not create ValidModule."),
    );

    let module = WasmInstance::from_valid_module_with_ctx(
        valid_module,
        mock_context(deployment, data_source, store.subgraph_store()),
        host_metrics,
        None,
        experimental_features,
    )
    .expect("Could not create WasmInstance from valid module with context.");

    let run_tests = module
        .instance
        .get_func("runTests")
        .expect("Couldn't get wasm function 'runTests'.");
    run_tests
        .call(&[])
        .expect("Couldn't call wasm function 'runTests'.");

    info!(logger, "Program execution time: {:?}", now.elapsed());
}
