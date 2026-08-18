//! Сервис графа знаний (KAG-lite) без внешних серверов баз данных (ADR-6).

pub mod service;

pub use service::{
    make_node_id, EdgeWithLabels, GraphReasonResult, GraphSearchResult, GraphService, GraphStats,
    NodeWithNeighbors,
};
