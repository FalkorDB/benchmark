use crate::query::{Bolt, Query, QueryBuilder};
use rand::seq::SliceRandom;
use rand::{random, Rng};
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
    name_to_id: HashMap<String, u16>,
    catalog: Vec<QueryCatalogEntry>,
}

impl QueriesRepository {
    fn new() -> Self {
        QueriesRepository {
            read_queries: HashMap::new(),
            write_queries: HashMap::new(),
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
                self.read_queries
                    .insert(name, QueryGenerator::new(query_type, generator));
            }
            QueryType::Write => {
                self.write_queries
                    .insert(name, QueryGenerator::new(query_type, generator));
            }
        }
    }

    pub fn catalog(&self) -> Vec<QueryCatalogEntry> {
        self.catalog.clone()
    }

    pub fn random_query(
        &self,
        query_type: QueryType,
    ) -> Option<PreparedQuery> {
        let queries = match query_type {
            QueryType::Read => &self.read_queries,
            QueryType::Write => &self.write_queries,
        };
        let keys: Vec<&String> = queries.keys().collect();
        let mut rng = rand::thread_rng();
        keys.choose(&mut rng).map(|&key| {
            let generator = queries.get(key).unwrap();
            let q_id = *self.name_to_id.get(key).unwrap_or(&0);
            PreparedQuery::new(
                q_id,
                key.clone(),
                generator.query_type,
                generator.generate(),
            )
        })
    }
}

struct RandomUtil {
    vertices: i32,
    _edges: i32,
}

impl RandomUtil {
    fn random_vertex(&self) -> i32 {
        let mut rng = rand::thread_rng();
        rng.gen_range(1..=self.vertices)
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
        let query_type = if random::<f32>() < write_ratio {
            QueryType::Write
        } else {
            QueryType::Read
        };
        self.queries_repository.random_query(query_type)
    }
    pub fn new(
        vertices: i32,
        edges: i32,
        flavour: Flavour,
    ) -> UsersQueriesRepository {
        let queries_repository = QueriesRepositoryBuilder::new(vertices, edges)
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
            })
            .build();

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
}
