use crate::queries_repository::{
    AlgorithmQuerySelection, QueryCatalogEntry, QueryCoverageProfile, QueryType,
};
use crate::tigergraph_query::{PreparedTigerGraphQuery, TigerGraphOperation, TigerGraphQueryBuilder};
use rand::prelude::IndexedRandom;
use std::collections::HashMap;

const ALGORITHM_QUERY_TARGET_RATIO_PER_QUERY: f32 = 0.01;
const ALGORITHM_QUERY_NAMES: [&str; 4] = [
    "algo_pagerank_summary",
    "algo_max_flow_single_pair",
    "algo_msf_summary",
    "algo_harmonic_summary",
];

fn is_algorithm_query_name(name: &str) -> bool {
    ALGORITHM_QUERY_NAMES.contains(&name)
}

struct RandomUtil {
    vertices: i32,
}

impl RandomUtil {
    fn random_vertex(&self) -> i32 {
        rand::random_range(1..=self.vertices)
    }

    fn random_path(&self) -> (i32, i32) {
        let start = self.random_vertex();
        let mut end = self.random_vertex();
        while end == start {
            end = self.random_vertex();
        }
        (start, end)
    }
}

type TigerGraphQueryFn = Box<dyn Fn() -> TigerGraphOperation + Send + Sync>;
type TigerGraphQueryEntry = (String, QueryType, TigerGraphQueryFn);

struct TigerGraphQueryGenerator {
    query_type: QueryType,
    generator: TigerGraphQueryFn,
}

impl TigerGraphQueryGenerator {
    fn generate(&self) -> TigerGraphOperation {
        (self.generator)()
    }
}

/// TigerGraph analogue of `queries_repository::QueriesRepository`/`PostgresQueriesRepository`.
/// Unlike Postgres/Mongo, TigerGraph natively supports the algorithm-procedure query family, so —
/// like the Cypher `QueriesRepository` — this tracks algorithm vs. non-algorithm read queries
/// separately to preserve the same rare-but-present algorithm sampling behaviour.
pub struct TigerGraphQueriesRepository {
    read_queries: HashMap<String, TigerGraphQueryGenerator>,
    write_queries: HashMap<String, TigerGraphQueryGenerator>,
    read_query_names: Vec<String>,
    write_query_names: Vec<String>,
    algorithm_read_query_names: Vec<String>,
    non_algorithm_read_query_names: Vec<String>,
    name_to_id: HashMap<String, u16>,
    catalog: Vec<QueryCatalogEntry>,
}

impl TigerGraphQueriesRepository {
    fn new() -> Self {
        TigerGraphQueriesRepository {
            read_queries: HashMap::new(),
            write_queries: HashMap::new(),
            read_query_names: Vec::new(),
            write_query_names: Vec::new(),
            algorithm_read_query_names: Vec::new(),
            non_algorithm_read_query_names: Vec::new(),
            name_to_id: HashMap::new(),
            catalog: Vec::new(),
        }
    }

    fn add_with_id(
        &mut self,
        id: u16,
        name: String,
        query_type: QueryType,
        generator: TigerGraphQueryFn,
    ) {
        self.name_to_id.insert(name.clone(), id);
        self.catalog.push(QueryCatalogEntry {
            id,
            name: name.clone(),
            q_type: query_type,
        });

        match query_type {
            QueryType::Read => {
                self.read_query_names.push(name.clone());
                if is_algorithm_query_name(name.as_str()) {
                    self.algorithm_read_query_names.push(name.clone());
                } else {
                    self.non_algorithm_read_query_names.push(name.clone());
                }
                self.read_queries
                    .insert(name, TigerGraphQueryGenerator { query_type, generator });
            }
            QueryType::Write => {
                self.write_query_names.push(name.clone());
                self.write_queries
                    .insert(name, TigerGraphQueryGenerator { query_type, generator });
            }
        }
    }

    pub fn catalog(&self) -> Vec<QueryCatalogEntry> {
        self.catalog.clone()
    }

    fn random_query_from_pool(
        &self,
        queries: &HashMap<String, TigerGraphQueryGenerator>,
        query_names: &[String],
    ) -> Option<PreparedTigerGraphQuery> {
        let mut rng = rand::rng();
        let key = query_names.choose(&mut rng)?;
        let generator = queries.get(key)?;
        let q_id = *self.name_to_id.get(key).unwrap_or(&0);
        Some(PreparedTigerGraphQuery::new(
            q_id,
            key.clone(),
            generator.query_type,
            generator.generate(),
        ))
    }

    pub fn random_query(
        &self,
        query_type: QueryType,
    ) -> Option<PreparedTigerGraphQuery> {
        let (queries, query_names) = match query_type {
            QueryType::Read => (&self.read_queries, &self.read_query_names),
            QueryType::Write => (&self.write_queries, &self.write_query_names),
        };
        self.random_query_from_pool(queries, query_names)
    }

    fn random_algorithm_read_query(&self) -> Option<PreparedTigerGraphQuery> {
        self.random_query_from_pool(&self.read_queries, &self.algorithm_read_query_names)
    }

    fn random_non_algorithm_read_query(&self) -> Option<PreparedTigerGraphQuery> {
        self.random_query_from_pool(&self.read_queries, &self.non_algorithm_read_query_names)
    }

    fn algorithm_read_query_count(&self) -> usize {
        self.algorithm_read_query_names.len()
    }
}

struct TigerGraphQueriesRepositoryBuilder {
    vertices: i32,
    queries: Vec<TigerGraphQueryEntry>,
}

impl TigerGraphQueriesRepositoryBuilder {
    fn new(vertices: i32) -> Self {
        TigerGraphQueriesRepositoryBuilder {
            vertices,
            queries: Vec::new(),
        }
    }

    fn add_query<F>(
        mut self,
        name: impl Into<String>,
        query_type: QueryType,
        generator: F,
    ) -> Self
    where
        F: Fn(&RandomUtil) -> TigerGraphOperation + Send + Sync + 'static,
    {
        let vertices = self.vertices;
        self.queries.push((
            name.into(),
            query_type,
            Box::new(move || {
                let random = RandomUtil { vertices };
                generator(&random)
            }),
        ));
        self
    }

    fn build(self) -> TigerGraphQueriesRepository {
        let mut repo = TigerGraphQueriesRepository::new();
        for (idx, (name, query_type, generator)) in self.queries.into_iter().enumerate() {
            repo.add_with_id(idx as u16, name, query_type, generator);
        }
        repo
    }
}

pub struct TigerGraphUsersQueriesRepository {
    repo: TigerGraphQueriesRepository,
}

impl TigerGraphUsersQueriesRepository {
    pub fn catalog(&self) -> Vec<QueryCatalogEntry> {
        self.repo.catalog()
    }

    pub fn random_queries(
        self,
        count: usize,
        write_ratio: f32,
    ) -> Box<dyn Iterator<Item = PreparedTigerGraphQuery> + Send + Sync> {
        Box::new((0..count).filter_map(move |_| self.random_query(write_ratio)))
    }

    /// Mirrors `UsersQueriesRepository::random_query`'s algorithm-aware weighting (rather than
    /// Postgres/Mongo's plain write-ratio split, which has no algorithm pool to weigh against):
    /// algorithm queries are drawn at a small fixed share, and the configured write ratio is
    /// preserved over the remaining (non-algorithm) portion.
    pub fn random_query(
        &self,
        write_ratio: f32,
    ) -> Option<PreparedTigerGraphQuery> {
        let algorithm_share = (self.repo.algorithm_read_query_count() as f32
            * ALGORITHM_QUERY_TARGET_RATIO_PER_QUERY)
            .clamp(0.0, 1.0);

        if rand::random::<f32>() < algorithm_share {
            if let Some(query) = self.repo.random_algorithm_read_query() {
                return Some(query);
            }
        }

        let remaining_share = 1.0 - algorithm_share;
        let capped_write_ratio = write_ratio.clamp(0.0, 1.0).min(remaining_share);
        let write_probability_within_remaining = if remaining_share > 0.0 {
            capped_write_ratio / remaining_share
        } else {
            0.0
        };

        if rand::random::<f32>() < write_probability_within_remaining {
            return self
                .repo
                .random_query(QueryType::Write)
                .or_else(|| self.repo.random_non_algorithm_read_query())
                .or_else(|| self.repo.random_query(QueryType::Read));
        }

        self.repo
            .random_non_algorithm_read_query()
            .or_else(|| self.repo.random_query(QueryType::Read))
            .or_else(|| self.repo.random_query(QueryType::Write))
    }

    /// Build the TigerGraph query catalog: the same baseline/extended-core family set Postgres
    /// supports (see `postgres_queries_repository.rs`), plus the four algorithm families (gated
    /// individually by `algorithm_selection`, mirroring the Cypher `--enable-algo-*` flags) that
    /// TigerGraph — unlike Postgres/Mongo — can express natively in GSQL. The `fixture-dependent`
    /// profile (vector/fulltext smoke queries) and `entity_path_introspection` have no GSQL
    /// equivalent and are intentionally omitted regardless of `query_coverage_profile`; callers
    /// are expected to reject `fixture-dependent` for TigerGraph before reaching this constructor.
    pub fn new(
        vertices: i32,
        _edges: i32,
        algorithm_selection: AlgorithmQuerySelection,
        query_coverage_profile: QueryCoverageProfile,
    ) -> TigerGraphUsersQueriesRepository {
        let mut builder = TigerGraphQueriesRepositoryBuilder::new(vertices)
            .add_query("single_vertex_read", QueryType::Read, |random| {
                TigerGraphQueryBuilder::new("single_vertex_read")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("single_vertex_write", QueryType::Write, |random| {
                TigerGraphQueryBuilder::new("single_vertex_write")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("single_vertex_update", QueryType::Write, |random| {
                TigerGraphQueryBuilder::new("single_vertex_update")
                    .param("id", random.random_vertex())
                    .param("rpc_social_credit", random.random_vertex())
                    .build()
            })
            .add_query("single_edge_update", QueryType::Write, |random| {
                TigerGraphQueryBuilder::new("single_edge_update")
                    .param("seed_id", random.random_vertex())
                    .param("color", random.random_vertex())
                    .build()
            })
            .add_query("single_edge_write", QueryType::Write, |random| {
                let (from, to) = random.random_path();
                TigerGraphQueryBuilder::new("single_edge_write")
                    .param("from_id", from)
                    .param("to_id", to)
                    .build()
            })
            .add_query("aggregate_expansion_1", QueryType::Read, |random| {
                TigerGraphQueryBuilder::new("aggregate_expansion_1")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("aggregate_expansion_1_with_filter", QueryType::Read, |random| {
                TigerGraphQueryBuilder::new("aggregate_expansion_1_with_filter")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("aggregate_expansion_2", QueryType::Read, |random| {
                TigerGraphQueryBuilder::new("aggregate_expansion_2")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("aggregate_expansion_2_with_filter", QueryType::Read, |random| {
                TigerGraphQueryBuilder::new("aggregate_expansion_2_with_filter")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("aggregate_expansion_3", QueryType::Read, |random| {
                TigerGraphQueryBuilder::new("aggregate_expansion_3")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("aggregate_expansion_3_with_filter", QueryType::Read, |random| {
                TigerGraphQueryBuilder::new("aggregate_expansion_3_with_filter")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("aggregate_expansion_4", QueryType::Read, |random| {
                TigerGraphQueryBuilder::new("aggregate_expansion_4")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("aggregate_expansion_4_with_filter", QueryType::Read, |random| {
                TigerGraphQueryBuilder::new("aggregate_expansion_4_with_filter")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("aggregate_age", QueryType::Read, |_random| {
                TigerGraphQueryBuilder::new("aggregate_age").build()
            })
            .add_query("aggregate_age_distinct", QueryType::Read, |_random| {
                TigerGraphQueryBuilder::new("aggregate_age_distinct").build()
            })
            .add_query("aggregate_age_filtered", QueryType::Read, |_random| {
                TigerGraphQueryBuilder::new("aggregate_age_filtered").build()
            })
            .add_query("aggregate_count_users", QueryType::Read, |_random| {
                TigerGraphQueryBuilder::new("aggregate_count_users").build()
            })
            .add_query("aggregate_age_min_max_avg", QueryType::Read, |_random| {
                TigerGraphQueryBuilder::new("aggregate_age_min_max_avg").build()
            })
            .add_query("neighbours_2", QueryType::Read, |random| {
                TigerGraphQueryBuilder::new("neighbours_2")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("neighbours_2_with_filter", QueryType::Read, |random| {
                TigerGraphQueryBuilder::new("neighbours_2_with_filter")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("neighbours_2_with_data", QueryType::Read, |random| {
                TigerGraphQueryBuilder::new("neighbours_2_with_data")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("neighbours_2_with_data_and_filter", QueryType::Read, |random| {
                TigerGraphQueryBuilder::new("neighbours_2_with_data_and_filter")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("shortest_path", QueryType::Read, |random| {
                let (from, to) = random.random_path();
                TigerGraphQueryBuilder::new("shortest_path")
                    .param("from_id", from)
                    .param("to_id", to)
                    .build()
            })
            .add_query("shortest_path_with_filter", QueryType::Read, |random| {
                let (from, to) = random.random_path();
                TigerGraphQueryBuilder::new("shortest_path_with_filter")
                    .param("from_id", from)
                    .param("to_id", to)
                    .build()
            })
            .add_query("pattern_cycle", QueryType::Read, |random| {
                TigerGraphQueryBuilder::new("pattern_cycle")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("pattern_long", QueryType::Read, |random| {
                TigerGraphQueryBuilder::new("pattern_long")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("pattern_short", QueryType::Read, |random| {
                TigerGraphQueryBuilder::new("pattern_short")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("vertex_on_label_property", QueryType::Read, |random| {
                TigerGraphQueryBuilder::new("vertex_on_label_property")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("vertex_on_label_property_index", QueryType::Read, |random| {
                TigerGraphQueryBuilder::new("vertex_on_label_property_index")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("vertex_on_property", QueryType::Read, |random| {
                TigerGraphQueryBuilder::new("vertex_on_property")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("value_join", QueryType::Read, |random| {
                TigerGraphQueryBuilder::new("value_join")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("value_join_cnt", QueryType::Read, |random| {
                TigerGraphQueryBuilder::new("value_join_cnt")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("order_by_age", QueryType::Read, |_random| {
                TigerGraphQueryBuilder::new("order_by_age").build()
            })
            .add_query("unwind_rows", QueryType::Read, |random| {
                TigerGraphQueryBuilder::new("unwind_rows")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("var_len_friends", QueryType::Read, |random| {
                TigerGraphQueryBuilder::new("var_len_friends")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("optional_friend", QueryType::Read, |random| {
                TigerGraphQueryBuilder::new("optional_friend")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("call_subquery", QueryType::Read, |random| {
                TigerGraphQueryBuilder::new("call_subquery")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("id_seek", QueryType::Read, |random| {
                TigerGraphQueryBuilder::new("id_seek")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("id_range_scan", QueryType::Read, |random| {
                let start = random.random_vertex();
                TigerGraphQueryBuilder::new("id_range_scan")
                    .param("start_id", start)
                    .param("end_id", start + 100)
                    .build()
            })
            .add_query("merge_user_insert_path", QueryType::Write, |random| {
                let insert_id = random.vertices.saturating_add(random.random_vertex());
                TigerGraphQueryBuilder::new("merge_user_insert_path")
                    .param("id", insert_id)
                    .param("age", random.random_vertex())
                    .build()
            })
            .add_query("merge_user_upsert_existing", QueryType::Write, |random| {
                TigerGraphQueryBuilder::new("merge_user_upsert_existing")
                    .param("id", random.random_vertex())
                    .param("age", random.random_vertex())
                    .build()
            })
            .add_query("merge_friend_edge_upsert", QueryType::Write, |random| {
                let (from, to) = random.random_path();
                TigerGraphQueryBuilder::new("merge_friend_edge_upsert")
                    .param("from_id", from)
                    .param("to_id", to)
                    .build()
            })
            .add_query("detach_delete_user", QueryType::Write, |random| {
                TigerGraphQueryBuilder::new("detach_delete_user")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("remove_user_property_and_label", QueryType::Write, |random| {
                TigerGraphQueryBuilder::new("remove_user_property_and_label")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("foreach_loop_mutation", QueryType::Write, |random| {
                TigerGraphQueryBuilder::new("foreach_loop_mutation")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("union_all_ids", QueryType::Read, |random| {
                TigerGraphQueryBuilder::new("union_all_ids")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("union_distinct_ids", QueryType::Read, |random| {
                TigerGraphQueryBuilder::new("union_distinct_ids")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("all_shortest_paths_len", QueryType::Read, |random| {
                let (from, to) = random.random_path();
                TigerGraphQueryBuilder::new("all_shortest_paths_len")
                    .param("from_id", from)
                    .param("to_id", to)
                    .build()
            })
            .add_query("var_len_with_edge_where_filter", QueryType::Read, |random| {
                TigerGraphQueryBuilder::new("var_len_with_edge_where_filter")
                    .param("id", random.random_vertex())
                    .param("min_capacity", 1)
                    .build()
            })
            .add_query("count_users_plain", QueryType::Read, |_random| {
                TigerGraphQueryBuilder::new("count_users_plain").build()
            })
            .add_query("count_friend_edges_plain", QueryType::Read, |_random| {
                TigerGraphQueryBuilder::new("count_friend_edges_plain").build()
            })
            .add_query("indexed_or_predicate", QueryType::Read, |random| {
                let (id1, id2) = random.random_path();
                TigerGraphQueryBuilder::new("indexed_or_predicate")
                    .param("id1", id1)
                    .param("id2", id2)
                    .build()
            })
            .add_query("indexed_in_list_predicate", QueryType::Read, |random| {
                TigerGraphQueryBuilder::new("indexed_in_list_predicate")
                    .param("id1", random.random_vertex())
                    .param("id2", random.random_vertex())
                    .param("id3", random.random_vertex())
                    .param("id4", random.random_vertex())
                    .build()
            });

        if query_coverage_profile.includes_extended_core() {
            builder = builder
                .add_query("exact_5_hop_traverse_count", QueryType::Read, |random| {
                    TigerGraphQueryBuilder::new("exact_5_hop_traverse_count")
                        .param("id", random.random_vertex())
                        .build()
                })
                .add_query("exact_6_hop_traverse_count", QueryType::Read, |random| {
                    TigerGraphQueryBuilder::new("exact_6_hop_traverse_count")
                        .param("id", random.random_vertex())
                        .build()
                })
                .add_query("temporal_spatial_roundtrip", QueryType::Read, |_random| {
                    TigerGraphQueryBuilder::new("temporal_spatial_roundtrip").build()
                });
        }

        if algorithm_selection.pagerank {
            builder = builder.add_query("algo_pagerank_summary", QueryType::Read, |_random| {
                TigerGraphQueryBuilder::new("algo_pagerank_summary").build()
            });
        }
        if algorithm_selection.max_flow {
            builder = builder.add_query("algo_max_flow_single_pair", QueryType::Read, |random| {
                let (source_id, target_id) = random.random_path();
                TigerGraphQueryBuilder::new("algo_max_flow_single_pair")
                    .param("source_id", source_id)
                    .param("target_id", target_id)
                    .build()
            });
        }
        if algorithm_selection.msf {
            builder = builder.add_query("algo_msf_summary", QueryType::Read, |random| {
                TigerGraphQueryBuilder::new("algo_msf_summary")
                    .param("source_id", random.random_vertex())
                    .build()
            });
        }
        if algorithm_selection.harmonic {
            builder = builder.add_query("algo_harmonic_summary", QueryType::Read, |_random| {
                TigerGraphQueryBuilder::new("algo_harmonic_summary").build()
            });
        }

        let repo = builder.build();
        TigerGraphUsersQueriesRepository { repo }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_catalog_excludes_unsupported_families() {
        let repo = TigerGraphUsersQueriesRepository::new(
            1000,
            10000,
            AlgorithmQuerySelection::default(),
            QueryCoverageProfile::Baseline,
        );
        let names: Vec<String> = repo.catalog().into_iter().map(|c| c.name).collect();
        assert!(!names.contains(&"vector_query_nodes_smoke".to_string()));
        assert!(!names.contains(&"entity_path_introspection".to_string()));
        assert!(!names.contains(&"exact_5_hop_traverse_count".to_string()));
        // TigerGraph-only additions (unlike Postgres/Mongo) must be present at baseline.
        assert!(names.contains(&"shortest_path".to_string()));
        assert!(names.contains(&"shortest_path_with_filter".to_string()));
        assert!(names.contains(&"all_shortest_paths_len".to_string()));
        assert!(names.contains(&"pattern_cycle".to_string()));
        assert!(names.contains(&"algo_pagerank_summary".to_string()));
        assert!(names.contains(&"algo_max_flow_single_pair".to_string()));
        assert!(names.contains(&"algo_msf_summary".to_string()));
        assert!(names.contains(&"algo_harmonic_summary".to_string()));
    }

    #[test]
    fn extended_core_adds_extra_queries() {
        let repo = TigerGraphUsersQueriesRepository::new(
            1000,
            10000,
            AlgorithmQuerySelection::default(),
            QueryCoverageProfile::ExtendedCore,
        );
        let names: Vec<String> = repo.catalog().into_iter().map(|c| c.name).collect();
        assert!(names.contains(&"exact_5_hop_traverse_count".to_string()));
        assert!(names.contains(&"exact_6_hop_traverse_count".to_string()));
        assert!(names.contains(&"temporal_spatial_roundtrip".to_string()));
    }

    #[test]
    fn algorithm_selection_can_disable_individual_families() {
        let repo = TigerGraphUsersQueriesRepository::new(
            1000,
            10000,
            AlgorithmQuerySelection {
                pagerank: true,
                max_flow: false,
                msf: false,
                harmonic: false,
            },
            QueryCoverageProfile::Baseline,
        );
        let names: Vec<String> = repo.catalog().into_iter().map(|c| c.name).collect();
        assert!(names.contains(&"algo_pagerank_summary".to_string()));
        assert!(!names.contains(&"algo_max_flow_single_pair".to_string()));
        assert!(!names.contains(&"algo_msf_summary".to_string()));
        assert!(!names.contains(&"algo_harmonic_summary".to_string()));
        assert_eq!(repo.repo.algorithm_read_query_count(), 1);
    }

    #[test]
    fn random_query_generation_produces_installed_query_operations() {
        let repo = TigerGraphUsersQueriesRepository::new(
            1000,
            10000,
            AlgorithmQuerySelection::default(),
            QueryCoverageProfile::Baseline,
        );
        for _ in 0..50 {
            let q = repo.random_query(0.5).expect("expected a query");
            match &q.operation {
                TigerGraphOperation::RunInstalledQuery { name, .. } => assert!(!name.is_empty()),
            }
        }
    }
}
