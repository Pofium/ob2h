//! Сервис графа знаний (KAG-lite) без внешних серверов баз данных (ADR-6).

pub mod analytics;
pub mod service;

pub use analytics::{
    AffectedNode, CircularDependency, ComponentMetrics, GodNodeInfo, GraphAnalytics, ImpactReport,
    ProjectReport, RiskLevel,
};
pub use service::{
    make_node_id, EdgeWithLabels, GraphReasonResult, GraphSearchResult, GraphService, GraphStats,
    NodeWithNeighbors, ScoredProjectNode,
};
