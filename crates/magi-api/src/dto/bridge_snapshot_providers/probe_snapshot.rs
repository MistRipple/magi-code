use magi_bridge_client::{BridgeServerKind, BridgeTransport, JsonRpcBridgeServerProbeClient};
use std::sync::Arc;

use super::super::bridges::{
    BridgeProbeErrorDto, BridgeServiceSnapshotDto, BridgeServicesSnapshotDto,
    BridgeSnapshotProvider,
};

#[derive(Clone, Default)]
pub struct BridgeProbeSnapshotProvider {
    probes: Vec<BridgeProbeBinding>,
}

impl BridgeProbeSnapshotProvider {
    pub fn register_probe(
        &mut self,
        server_kind: BridgeServerKind,
        probe: JsonRpcBridgeServerProbeClient,
    ) {
        self.probes
            .retain(|binding| binding.server_kind != server_kind);
        self.probes.push(BridgeProbeBinding { server_kind, probe });
    }

    pub fn register_transport(
        &mut self,
        server_kind: BridgeServerKind,
        transport: Arc<dyn BridgeTransport>,
    ) {
        self.register_probe(server_kind, JsonRpcBridgeServerProbeClient::new(transport));
    }

    fn capture_binding_snapshot(binding: &BridgeProbeBinding) -> BridgeServiceSnapshotDto {
        let (handshake, handshake_error) = match binding.probe.handshake() {
            Ok(handshake) => (Some(handshake), None),
            Err(error) => (None, Some(BridgeProbeErrorDto::from_client_error(error))),
        };
        let (health, health_error) = match binding.probe.health() {
            Ok(health) => (Some(health), None),
            Err(error) => (None, Some(BridgeProbeErrorDto::from_client_error(error))),
        };
        let (service_catalog, service_catalog_error) = match binding.probe.describe_services() {
            Ok(service_catalog) => (Some(service_catalog), None),
            Err(error) => (None, Some(BridgeProbeErrorDto::from_client_error(error))),
        };

        BridgeServiceSnapshotDto {
            server_kind: binding.server_kind,
            handshake,
            handshake_error,
            health,
            health_error,
            service_catalog,
            service_catalog_error,
        }
    }
}

impl BridgeSnapshotProvider for BridgeProbeSnapshotProvider {
    fn services_snapshot(&self) -> BridgeServicesSnapshotDto {
        BridgeServicesSnapshotDto {
            services: self
                .probes
                .iter()
                .map(Self::capture_binding_snapshot)
                .collect(),
        }
    }
}

#[derive(Clone)]
struct BridgeProbeBinding {
    server_kind: BridgeServerKind,
    probe: JsonRpcBridgeServerProbeClient,
}
