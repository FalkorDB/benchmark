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
    _Neo4j,
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

        for (name, query_type, generator) in self.queries {
            queries_repository.add(name, query_type, generator);
        }
        queries_repository
    }
}

pub struct QueriesRepository {
    read_queries: HashMap<String, QueryGenerator>,
    write_queries: HashMap<String, QueryGenerator>,
}

impl QueriesRepository {
    fn new() -> Self {
        QueriesRepository {
            read_queries: HashMap::new(),
            write_queries: HashMap::new(),
        }
    }

    fn add<F>(
        &mut self,
        name: impl Into<String>,
        query_type: QueryType,
        generator: F,
    ) where
        F: Fn() -> Query + Send + Sync + 'static,
    {
        match query_type {
            QueryType::Read => {
                self.read_queries
                    .insert(name.into(), QueryGenerator::new(query_type, generator));
            }
            QueryType::Write => {
                self.write_queries
                    .insert(name.into(), QueryGenerator::new(query_type, generator));
            }
        }
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
            PreparedQuery::new(key.clone(), generator.query_type, generator.generate())
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
    ) -> UsersQueriesRepository {
        let queries_repository = QueriesRepositoryBuilder::new(vertices, edges)
            .flavour(Flavour::FalkorDB)
            .add_query("single_vertex_read", QueryType::Read, |random, _flavour| {
                QueryBuilder::new()
                    .text("MATCH (n:User {id : $id}) RETURN n")
                    .param("id", random.random_vertex())
                    .build()
            })
            // .add_query("single_vertex_write", QueryType::Write, |random, _flavour| {
            //     QueryBuilder::new()
            //         .text("CREATE (n:UserTemp {id : $id}) RETURN n")
            //         .param("id", random.random_vertex())
            //         .build()
            .add_query("single_vertex_update", QueryType::Write, |random, _flavour| {
                QueryBuilder::new()
                    .text("MATCH (n:User {id: $id}) SET n.rpc_social_credit = $rpc_social_credit RETURN n")
                    .param("id", random.random_vertex())
                    .param("rpc_social_credit", random.random_vertex())
                    .build()
            })
            .add_query("single_edge_update", QueryType::Write, |random, _flavour| {
                QueryBuilder::new()
                    .text("MATCH (n:User)-[e:Temp]->(m:User) WITH e ORDER BY rand() LIMIT 1 SET e.color = $color RETURN e")
                    .param("color", random.random_vertex())
                    .build()
            })
            // .add_query("single_edge_write", QueryType::Write, |random, _flavour| {
            //     let (from, to) = random.random_path();
            //     QueryBuilder::new()
            //         .text("MATCH (n:User {id: $from}), (m:User {id: $to}) WITH n, m CREATE (n)-[e:Temp]->(m) RETURN e")
            //         .param("from", from)
            //         .param("to", to)
            //         .build()
            // })
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
            .build();

        UsersQueriesRepository { queries_repository }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PreparedQuery {
    pub q_name: String,
    pub q_type: QueryType,
    pub query: Query,
    pub cypher: String,
    pub bolt: Bolt,
}

impl PreparedQuery {
    pub fn new(
        q_name: String,
        q_type: QueryType,
        query: Query,
    ) -> Self {
        let cypher = query.to_cypher();
        let bolt = query.to_bolt_struct();
        Self {
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
