use crate::query::{Bolt, Query, QueryBuilder};
use rand::prelude::IndexedRandom;
use rand::random;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, PartialEq, Eq, Clone, Copy, Serialize, Deserialize)]
pub enum QueryType {
    Read,
    Write,
}
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum Flavour {
    FalkorDB,
    Neo4j,
    Memgraph,
}

pub const NEO4J_ALGORITHM_GRAPH_NAME: &str = "benchmark_algo_graph";
const ALGORITHM_QUERY_TARGET_RATIO_PER_QUERY: f32 = 0.01;
const ALGORITHM_QUERY_NAMES: [&str; 4] = [
    "algo_pagerank_summary",
    "algo_max_flow_single_pair",
    "algo_msf_summary",
    "algo_harmonic_summary",
];

#[derive(Debug, Clone, Copy)]
pub struct AlgorithmQuerySelection {
    pub pagerank: bool,
    pub max_flow: bool,
    pub msf: bool,
    pub harmonic: bool,
}

impl Default for AlgorithmQuerySelection {
    fn default() -> Self {
        Self {
            pagerank: true,
            max_flow: true,
            msf: true,
            harmonic: true,
        }
    }
}

fn is_algorithm_query_name(name: &str) -> bool {
    ALGORITHM_QUERY_NAMES.contains(&name)
}

struct Empty;

pub struct QueryGenerator {
    query_type: QueryType,
    generator: Box<dyn Fn() -> Query + Send + Sync>,
}

impl QueryGenerator {
    pub fn new<F>(
        query_type: QueryType,
        generator: F,
    ) -> Self
    where
        F: Fn() -> Query + Send + Sync + 'static,
    {
        QueryGenerator {
            query_type,
            generator: Box::new(generator),
        }
    }

    pub fn generate(&self) -> Query {
        (self.generator)()
    }
}

// Define a type alias for the function type
type QueryFn = Box<dyn Fn() -> Query + Send + Sync>;

// Define a type alias for the tuple
type QueryEntry = (String, QueryType, QueryFn);

pub struct QueriesRepositoryBuilder<U: Send> {
    vertices: i32,
    edges: i32,
    queries: Vec<QueryEntry>,
    flavour: U,
}

impl QueriesRepositoryBuilder<Empty> {
    pub fn new(
        vertices: i32,
        edges: i32,
    ) -> QueriesRepositoryBuilder<Empty> {
        QueriesRepositoryBuilder {
            vertices,
            edges,
            queries: Vec::new(),
            flavour: Empty,
        }
    }
    pub fn flavour(
        self,
        flavour: Flavour,
    ) -> QueriesRepositoryBuilder<Flavour> {
        QueriesRepositoryBuilder {
            vertices: self.vertices,
            edges: self.edges,
            queries: self.queries,
            flavour,
        }
    }
}
impl QueriesRepositoryBuilder<Flavour> {
    fn add_query<F>(
        mut self,
        name: impl Into<String>,
        query_type: QueryType,
        generator: F,
    ) -> Self
    where
        F: Fn(&RandomUtil, Flavour) -> Query + Send + Sync + 'static,
    {
        let vertices = self.vertices;
        let edges = self.edges;
        let flavour = self.flavour;
        self.queries.push((
            name.into(),
            query_type,
            Box::new(move || {
                let random = RandomUtil {
                    vertices,
                    _edges: edges,
                };
                generator(&random, flavour)
            }),
        ));
        self
    }

    pub fn build(self) -> QueriesRepository {
        let mut queries_repository = QueriesRepository::new();

        for (idx, (name, query_type, generator)) in self.queries.into_iter().enumerate() {
            // Stable query ids are assigned in definition order.
            let id = idx as u16;
            queries_repository.add_with_id(id, name, query_type, generator);
        }
        queries_repository
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryCatalogEntry {
    pub id: u16,
    pub name: String,
    pub q_type: QueryType,
}

pub struct QueriesRepository {
    read_queries: HashMap<String, QueryGenerator>,
    write_queries: HashMap<String, QueryGenerator>,
    read_query_names: Vec<String>,
    write_query_names: Vec<String>,
    algorithm_read_query_names: Vec<String>,
    non_algorithm_read_query_names: Vec<String>,
    name_to_id: HashMap<String, u16>,
    catalog: Vec<QueryCatalogEntry>,
}

impl QueriesRepository {
    fn new() -> Self {
        QueriesRepository {
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

    fn add_with_id<F>(
        &mut self,
        id: u16,
        name: impl Into<String>,
        query_type: QueryType,
        generator: F,
    ) where
        F: Fn() -> Query + Send + Sync + 'static,
    {
        let name = name.into();
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
                    .insert(name, QueryGenerator::new(query_type, generator));
            }
            QueryType::Write => {
                self.write_query_names.push(name.clone());
                self.write_queries
                    .insert(name, QueryGenerator::new(query_type, generator));
            }
        }
    }

    pub fn catalog(&self) -> Vec<QueryCatalogEntry> {
        self.catalog.clone()
    }

    fn random_query_from_pool(
        &self,
        queries: &HashMap<String, QueryGenerator>,
        query_names: &[String],
    ) -> Option<PreparedQuery> {
        let mut rng = rand::rng();
        let key = query_names.choose(&mut rng)?;
        let generator = queries.get(key)?;
        let q_id = *self.name_to_id.get(key).unwrap_or(&0);
        Some(PreparedQuery::new(
            q_id,
            key.clone(),
            generator.query_type,
            generator.generate(),
        ))
    }

    pub fn random_query(
        &self,
        query_type: QueryType,
    ) -> Option<PreparedQuery> {
        let (queries, query_names) = match query_type {
            QueryType::Read => (&self.read_queries, &self.read_query_names),
            QueryType::Write => (&self.write_queries, &self.write_query_names),
        };
        self.random_query_from_pool(queries, query_names)
    }

    fn random_algorithm_read_query(&self) -> Option<PreparedQuery> {
        self.random_query_from_pool(&self.read_queries, &self.algorithm_read_query_names)
    }

    fn random_non_algorithm_read_query(&self) -> Option<PreparedQuery> {
        self.random_query_from_pool(&self.read_queries, &self.non_algorithm_read_query_names)
    }

    fn algorithm_read_query_count(&self) -> usize {
        self.algorithm_read_query_names.len()
    }
}

struct RandomUtil {
    vertices: i32,
    _edges: i32,
}

impl RandomUtil {
    fn random_vertex(&self) -> i32 {
        rand::random_range(1..=self.vertices)
    }
    #[allow(dead_code)]
    fn random_path(&self) -> (i32, i32) {
        let start = self.random_vertex();
        let mut end = self.random_vertex();

        // Ensure start and end are different
        while end == start {
            end = self.random_vertex();
        }
        (start, end)
    }
}
pub struct UsersQueriesRepository {
    queries_repository: QueriesRepository,
}

impl UsersQueriesRepository {
    pub fn catalog(&self) -> Vec<QueryCatalogEntry> {
        self.queries_repository.catalog()
    }

    pub fn random_queries(
        self,
        count: usize,
        write_ratio: f32,
    ) -> Box<dyn Iterator<Item = PreparedQuery> + Send + Sync> {
        Box::new((0..count).filter_map(move |_| self.random_query(write_ratio)))
    }
    pub fn random_query(
        &self,
        write_ratio: f32,
    ) -> Option<PreparedQuery> {
        let algorithm_share = (self.queries_repository.algorithm_read_query_count() as f32
            * ALGORITHM_QUERY_TARGET_RATIO_PER_QUERY)
            .clamp(0.0, 1.0);

        if random::<f32>() < algorithm_share {
            if let Some(query) = self.queries_repository.random_algorithm_read_query() {
                return Some(query);
            }
        }

        // Preserve the configured write ratio over the non-algorithm portion.
        let remaining_share = 1.0 - algorithm_share;
        let capped_write_ratio = write_ratio.clamp(0.0, 1.0).min(remaining_share);
        let write_probability_within_remaining = if remaining_share > 0.0 {
            capped_write_ratio / remaining_share
        } else {
            0.0
        };

        if random::<f32>() < write_probability_within_remaining {
            return self
                .queries_repository
                .random_query(QueryType::Write)
                .or_else(|| self.queries_repository.random_non_algorithm_read_query())
                .or_else(|| self.queries_repository.random_query(QueryType::Read));
        }

        self.queries_repository
            .random_non_algorithm_read_query()
            .or_else(|| self.queries_repository.random_query(QueryType::Read))
            .or_else(|| self.queries_repository.random_query(QueryType::Write))
    }
    pub fn new(
        vertices: i32,
        edges: i32,
        flavour: Flavour,
        algorithm_selection: AlgorithmQuerySelection,
    ) -> UsersQueriesRepository {
        let mut queries_builder = QueriesRepositoryBuilder::new(vertices, edges)
            .flavour(flavour)
            .add_query("single_vertex_read", QueryType::Read, |random, _flavour| {
                QueryBuilder::new()
                    .text("MATCH (n:User {id : $id}) RETURN n")
                    .param("id", random.random_vertex())
                    .build()
            })
.add_query("single_vertex_write", QueryType::Write, |random, _flavour| {
                QueryBuilder::new()
                    .text("CREATE (n:User {id : $id}) RETURN n")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("single_vertex_update", QueryType::Write, |random, _flavour| {
                QueryBuilder::new()
                    .text("MATCH (n:User {id: $id}) SET n.rpc_social_credit = $rpc_social_credit RETURN n")
                    .param("id", random.random_vertex())
                    .param("rpc_social_credit", random.random_vertex())
                    .build()
            })
.add_query("single_edge_update", QueryType::Write, |random, _flavour| {
                QueryBuilder::new()
                    .text("MATCH (n:User)-[e:Friend]->(m:User) WITH e ORDER BY rand() LIMIT 1 SET e.color = $color RETURN e")
                    .param("color", random.random_vertex())
                    .build()
            })
.add_query("single_edge_write", QueryType::Write, |random, _flavour| {
                let (from, to) = random.random_path();
                QueryBuilder::new()
                    .text("MATCH (n:User {id: $from}), (m:User {id: $to}) WITH n, m CREATE (n)-[e:Friend]->(m) RETURN e")
                    .param("from", from)
                    .param("to", to)
                    .build()
            })
            .add_query("aggregate_expansion_1", QueryType::Read, |random, _flavour| {
                QueryBuilder::new()
                    .text("MATCH (s:User {id: $id})-->(n:User) RETURN n.id")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query(
                "aggregate_expansion_1_with_filter",
                QueryType::Read,
                |random, _flavour| {
                    QueryBuilder::new()
                        .text("MATCH (s:User {id: $id})-->(n:User)  WHERE n.age >= 18  RETURN n.id")
                        .param("id", random.random_vertex())
                        .build()
                },
            )
            .add_query("aggregate_expansion_2", QueryType::Read, |random, _flavour| {
                QueryBuilder::new()
                    .text("MATCH (s:User {id: $id})-->()-->(n:User) RETURN DISTINCT n.id")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query(
                "aggregate_expansion_2_with_filter",
                QueryType::Read,
                |random, _flavour| {
                    QueryBuilder::new()
                        .text("MATCH (s:User {id: $id})-->()-->(n:User)  WHERE n.age >= 18  RETURN DISTINCT n.id")
                        .param("id", random.random_vertex())
                        .build()
                },
            )
            .add_query(
                "aggregate_expansion_3",
                QueryType::Read,
                |random, _flavour| {
                    QueryBuilder::new()
                        .text("MATCH (s:User {id: $id})-->()-->()-->(n:User) RETURN DISTINCT n.id")
                        .param("id", random.random_vertex())
                        .build()
                },
            )
            .add_query(
                "aggregate_expansion_3_with_filter",
                QueryType::Read,
                |random, _flavour| {
                    QueryBuilder::new()
                        .text("MATCH (s:User {id: $id})-->()-->()-->(n:User)  WHERE n.age >= 18  RETURN DISTINCT n.id")
                        .param("id", random.random_vertex())
                        .build()
                },
            )
            .add_query(
                "aggregate_expansion_4",
                QueryType::Read,
                |random, _flavour| {
                    QueryBuilder::new()
                        .text("MATCH (s:User {id: $id})-->()-->()-->()-->(n:User) RETURN DISTINCT n.id")
                        .param("id", random.random_vertex())
                        .build()
                },
            )
            .add_query(
                "aggregate_expansion_4_with_filter",
                QueryType::Read,
                |random, _flavour| {
                    QueryBuilder::new()
                        .text("MATCH (s:User {id: $id})-->()-->()-->()-->(n:User)  WHERE n.age >= 18 RETURN DISTINCT n.id")
                        .param("id", random.random_vertex())
                        .build()
                },
            )
            // Aggregation queries aligned with mgbench Pokec workload
            .add_query("aggregate_age", QueryType::Read, |_random, _flavour| {
                QueryBuilder::new()
                    .text("MATCH (n:User) RETURN avg(n.age) AS avg_age")
                    .build()
            })
            .add_query("aggregate_age_distinct", QueryType::Read, |_random, _flavour| {
                QueryBuilder::new()
                    .text("MATCH (n:User) RETURN count(DISTINCT n.age) AS distinct_ages")
                    .build()
            })
            .add_query("aggregate_age_filtered", QueryType::Read, |_random, _flavour| {
                QueryBuilder::new()
                    .text("MATCH (n:User) WHERE n.age >= 18 RETURN avg(n.age) AS avg_age")
                    .build()
            })
.add_query("aggregate_count_users", QueryType::Read, |_random, flavour| {
                match flavour {
                    Flavour::FalkorDB => {
                        // Use FalkorDB's db.meta.stats() for fast global node count.
                        QueryBuilder::new()
                            .text("CALL db.meta.stats() YIELD nodeCount RETURN nodeCount AS cnt")
                            .build()
                    }
                    _ => {
                        QueryBuilder::new()
                            .text("MATCH (n:User) RETURN count(n) AS cnt")
                            .build()
                    }
                }
            })
            .add_query("aggregate_age_min_max_avg", QueryType::Read, |_random, _flavour| {
                QueryBuilder::new()
                    .text("MATCH (n:User) RETURN min(n.age) AS min_age, max(n.age) AS max_age, avg(n.age) AS avg_age")
                    .build()
            })
            // Neighbourhood queries (2-hop)
            .add_query("neighbours_2", QueryType::Read, |random, _flavour| {
                QueryBuilder::new()
                    .text("MATCH (s:User {id: $id})-->()-->(n:User) RETURN n.id")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("neighbours_2_with_filter", QueryType::Read, |random, _flavour| {
                QueryBuilder::new()
                    .text("MATCH (s:User {id: $id})-->()-->(n:User) WHERE n.age >= 18 RETURN n.id")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("neighbours_2_with_data", QueryType::Read, |random, _flavour| {
                QueryBuilder::new()
                    .text("MATCH (s:User {id: $id})-->()-->(n:User) RETURN n")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query(
                "neighbours_2_with_data_and_filter",
                QueryType::Read,
                |random, _flavour| {
                    QueryBuilder::new()
                        .text("MATCH (s:User {id: $id})-->()-->(n:User) WHERE n.age >= 18 RETURN n")
                        .param("id", random.random_vertex())
                        .build()
                },
            )
            // Shortest-path style queries
            .add_query("shortest_path", QueryType::Read, |random, flavour| {
                let (from, to) = random.random_path();
                let text = match flavour {
                    Flavour::FalkorDB => "MATCH (s:User {id: $from}), (t:User {id: $to}) WITH shortestPath((s)-[*]->(t)) AS p RETURN length(p)",
                    Flavour::Neo4j => "MATCH (s:User {id: $from}), (t:User {id: $to}) MATCH p = shortestPath((s)-[*]->(t)) RETURN length(p)",
                    Flavour::Memgraph => "MATCH p = (:User {id: $from})-[*BFS]->(:User {id: $to}) RETURN length(p)",
                };
                QueryBuilder::new()
                    .text(text)
                    .param("from", from)
                    .param("to", to)
                    .build()
            })
            .add_query("shortest_path_with_filter", QueryType::Read, |random, flavour| {
                let (from, to) = random.random_path();
                let text = match flavour {
                    Flavour::FalkorDB => "MATCH (s:User {id: $from}), (t:User {id: $to}) WITH shortestPath((s)-[*]->(t)) AS p WHERE length(p) > 0 RETURN length(p)",
                    Flavour::Neo4j => "MATCH (s:User {id: $from}), (t:User {id: $to}) MATCH p = shortestPath((s)-[*]->(t)) WHERE length(p) > 0 RETURN length(p)",
                    Flavour::Memgraph => "MATCH p = (:User {id: $from})-[*BFS]->(:User {id: $to}) WHERE length(p) > 0 RETURN length(p)",
                };
                QueryBuilder::new()
                    .text(text)
                    .param("from", from)
                    .param("to", to)
                    .build()
            })
            // Pattern and index-based queries
            .add_query("pattern_cycle", QueryType::Read, |random, _flavour| {
                QueryBuilder::new()
                    .text("MATCH (a:User {id: $id})-->(b:User)-->(c:User)-->(a) RETURN a.id, b.id, c.id")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("pattern_long", QueryType::Read, |random, _flavour| {
                QueryBuilder::new()
                    .text("MATCH (a:User {id: $id})-->()-->()-->()-->(b:User) RETURN a.id, b.id")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("pattern_short", QueryType::Read, |random, _flavour| {
                QueryBuilder::new()
                    .text("MATCH (a:User {id: $id})-->()-->(b:User) RETURN a.id, b.id")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("vertex_on_label_property", QueryType::Read, |random, _flavour| {
                QueryBuilder::new()
                    .text("MATCH (n:User {id: $id}) RETURN n")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query(
                "vertex_on_label_property_index",
                QueryType::Read,
                |random, _flavour| {
                    QueryBuilder::new()
                        .text("MATCH (n:User {id: $id}) RETURN n")
                        .param("id", random.random_vertex())
                        .build()
                },
            )
            .add_query("vertex_on_property", QueryType::Read, |random, _flavour| {
                QueryBuilder::new()
                    .text("MATCH (n {id: $id}) RETURN n")
                    .param("id", random.random_vertex())
                    .build()
            });
        if algorithm_selection.pagerank {
            queries_builder = queries_builder.add_query(
                "algo_pagerank_summary",
                QueryType::Read,
                |_random, flavour| {
                    let text = match flavour {
                        Flavour::FalkorDB => "CALL algo.pageRank('User', null) \
                                             YIELD node, score \
                                             RETURN score \
                                             LIMIT 1",
                        Flavour::Neo4j => {
                            "CALL gds.pageRank.stream('benchmark_algo_graph') \
                             YIELD nodeId, score \
                             RETURN score \
                             LIMIT 1"
                        }
                        Flavour::Memgraph => {
                            "CALL pagerank.get() \
                             YIELD node, rank \
                             RETURN rank AS score \
                             LIMIT 1"
                        }
                    };
                    QueryBuilder::new().text(text).build()
                },
            );
        }

        if algorithm_selection.max_flow {
            queries_builder = queries_builder.add_query(
                "algo_max_flow_single_pair",
                QueryType::Read,
                |random, flavour| {
                    let (source_id, target_id) = random.random_path();
                    let text = match flavour {
                        Flavour::FalkorDB => {
                            "MATCH (s:User {id: $source_id}), (t:User {id: $target_id}) \
                             CALL db.relationshipTypes() YIELD relationshipType \
                             WITH s, t, collect(relationshipType) AS relationshipTypes \
                             CALL algo.maxFlow({ \
                                 sourceNodes: [s], \
                                 targetNodes: [t], \
                                 relationshipTypes: relationshipTypes, \
                                 capacityProperty: 'bench_capacity' \
                             }) \
                             YIELD maxFlow \
                             RETURN coalesce(toFloat(maxFlow), 0.0) AS max_flow"
                        }
                        Flavour::Neo4j => {
                            "MATCH (s:User {id: $source_id}), (t:User {id: $target_id}) \
                             CALL gds.maxFlow.stats('benchmark_algo_graph', { \
                                 sourceNodes: [s], \
                                 targetNodes: [t], \
                                 capacityProperty: 'bench_capacity' \
                             }) \
                             YIELD maxFlow \
                             RETURN coalesce(toFloat(maxFlow), 0.0) AS max_flow"
                        }
                        Flavour::Memgraph => {
                            "MATCH (s:User {id: $source_id}), (t:User {id: $target_id}) \
                             CALL max_flow.get_flow(s, t, 'bench_capacity') \
                             YIELD max_flow \
                             RETURN coalesce(toFloat(max_flow), 0.0) AS max_flow"
                        }
                    };
                    QueryBuilder::new()
                        .text(text)
                        .param("source_id", source_id)
                        .param("target_id", target_id)
                        .build()
                },
            );
        }

        if algorithm_selection.msf {
            queries_builder = queries_builder.add_query(
                "algo_msf_summary",
                QueryType::Read,
                |random, flavour| {
                    let source_id = random.random_vertex();
                    let text = match flavour {
                        Flavour::FalkorDB => {
                            "CALL algo.MSF({ \
                                 weightAttribute: 'bench_capacity' \
                             }) \
                             YIELD edges \
                             RETURN \
                                 size(edges) AS edge_count, \
                                 reduce(total = 0.0, edge IN edges | total + coalesce(toFloat(edge.bench_capacity), 0.0)) AS total_weight"
                        }
                        Flavour::Neo4j => {
                            "MATCH (source:User {id: $source_id}) \
                             CALL gds.spanningTree.stats('benchmark_algo_graph', { \
                                 sourceNode: source, \
                                 relationshipWeightProperty: 'bench_capacity' \
                             }) \
                             YIELD effectiveNodeCount, totalWeight \
                             RETURN \
                                 CASE WHEN effectiveNodeCount > 0 THEN effectiveNodeCount - 1 ELSE 0 END AS edge_count, \
                                 coalesce(totalWeight, 0.0) AS total_weight"
                        }
                        Flavour::Memgraph => {
                            "CALL igraphalg.spanning_tree('bench_capacity', false) \
                             YIELD tree \
                             RETURN \
                                 size(tree) AS edge_count, \
                                 0.0 AS total_weight"
                        }
                    };
                    QueryBuilder::new()
                        .text(text)
                        .param("source_id", source_id)
                        .build()
                },
            );
        }

        if algorithm_selection.harmonic {
            queries_builder = queries_builder.add_query(
                "algo_harmonic_summary",
                QueryType::Read,
                |_random, flavour| {
                    let text = match flavour {
                        Flavour::FalkorDB => {
                            "CALL algo.HarmonicCentrality() \
                             YIELD node, score \
                             RETURN count(node) AS node_count, avg(score) AS avg_score, max(score) AS max_score"
                        }
                        Flavour::Neo4j => {
                            "CALL gds.closeness.harmonic.stream('benchmark_algo_graph') \
                             YIELD nodeId, score \
                             RETURN count(nodeId) AS node_count, avg(score) AS avg_score, max(score) AS max_score"
                        }
                        Flavour::Memgraph => {
                            "CALL nxalg.harmonic_centrality() \
                             YIELD node, harmonic_centrality \
                             RETURN \
                                count(node) AS node_count, \
                                avg(harmonic_centrality) AS avg_score, \
                                max(harmonic_centrality) AS max_score"
                        }
                    };
                    QueryBuilder::new().text(text).build()
                },
            );
        }

        let queries_repository = queries_builder.build();

        UsersQueriesRepository { queries_repository }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PreparedQuery {
    #[serde(default)]
    pub q_id: u16,
    pub q_name: String,
    pub q_type: QueryType,
    pub query: Query,
    pub cypher: String,
    pub bolt: Bolt,
}

impl PreparedQuery {
    pub fn new(
        q_id: u16,
        q_name: String,
        q_type: QueryType,
        query: Query,
    ) -> Self {
        let cypher = query.to_cypher();
        let bolt = query.to_bolt_struct();
        Self {
            q_id,
            q_name,
            q_type,
            query,
            cypher,
            bolt,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_query_generator() {
        let generator = QueryGenerator::new(QueryType::Read, || {
            QueryBuilder::new()
                .text("MATCH (p:Person) RETURN p")
                .build()
        });

        let query = generator.generate();
        assert_eq!(query.text, "MATCH (p:Person) RETURN p");
    }

    #[test]
    fn test_algorithm_queries_are_tracked() {
        let repository = UsersQueriesRepository::new(
            100,
            1000,
            Flavour::FalkorDB,
            AlgorithmQuerySelection::default(),
        );
        assert_eq!(
            repository.queries_repository.algorithm_read_query_count(),
            ALGORITHM_QUERY_NAMES.len()
        );
    }

    #[test]
    fn test_algorithm_selection_can_limit_queries() {
        let repository = UsersQueriesRepository::new(
            100,
            1000,
            Flavour::FalkorDB,
            AlgorithmQuerySelection {
                pagerank: true,
                max_flow: false,
                msf: false,
                harmonic: false,
            },
        );

        assert_eq!(
            repository.queries_repository.algorithm_read_query_count(),
            1
        );
    }
}
