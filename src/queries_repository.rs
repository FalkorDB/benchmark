use crate::query::{Query, QueryBuilder};
use rand::seq::SliceRandom;
use rand::{Rng, RngCore, SeedableRng};
use rand_pcg::{Lcg64Xsh32, Pcg32};
use std::collections::HashMap;

pub(crate) trait Queries {
    fn random_query(&mut self) -> Option<(String, QueryType, Query)>;

    fn random_queries(
        &mut self,
        count: u64,
    ) -> Box<dyn Iterator<Item = (String, QueryType, Query)> + '_> {
        Box::new((0..count).filter_map(move |_| self.random_query()))
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum QueryType {
    Read,
    Write,
}
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub(crate) enum Flavour {
    FalkorDB,
    _Neo4j,
}

struct Empty;

pub struct QueryGenerator {
    query_type: QueryType,
    generator: Box<dyn Fn(&mut dyn RngCore) -> Query>,
}

impl QueryGenerator {
    pub fn new<F>(
        query_type: QueryType,
        generator: F,
    ) -> Self
    where
        F: Fn(&mut dyn RngCore) -> Query + 'static,
    {
        QueryGenerator {
            query_type,
            generator: Box::new(generator),
        }
    }

    pub fn generate(
        &self,
        rng: &mut dyn RngCore,
    ) -> Query {
        (self.generator)(rng)
    }
}

pub struct QueriesRepositoryBuilder<U> {
    vertices: i32,
    edges: i32,
    queries: Vec<(String, QueryType, Box<dyn Fn(&mut dyn RngCore) -> Query>)>,
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
    pub(crate) fn flavour(
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
        F: Fn(&RandomUtil, Flavour, &mut dyn RngCore) -> Query + 'static,
    {
        let vertices = self.vertices;
        let edges = self.edges;
        let flavour = self.flavour;
        self.queries.push((
            name.into(),
            query_type,
            Box::new(move |rng| {
                let random = RandomUtil {
                    vertices,
                    _edges: edges,
                };
                generator(&random, flavour, rng)
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
    queries: HashMap<String, QueryGenerator>,
    rng: Lcg64Xsh32,
}

impl QueriesRepository {
    fn new() -> Self {
        let seed: u64 = 42;
        let rng: Lcg64Xsh32 = Pcg32::seed_from_u64(seed);
        QueriesRepository {
            queries: HashMap::new(),
            rng,
        }
    }

    fn add<F>(
        &mut self,
        name: impl Into<String>,
        query_type: QueryType,
        generator: F,
    ) where
        F: Fn(&mut dyn RngCore) -> Query + 'static,
    {
        self.queries
            .insert(name.into(), QueryGenerator::new(query_type, generator));
    }
}

impl Queries for QueriesRepository {
    fn random_query(&mut self) -> Option<(String, QueryType, Query)> {
        let mut keys: Vec<&String> = self.queries.keys().collect();
        keys.sort();
        keys.choose(&mut self.rng).map(|&key| {
            let generator = self.queries.get(key).unwrap();
            (
                key.clone(),
                generator.query_type,
                generator.generate(&mut self.rng),
            )
        })
    }
}

struct RandomUtil {
    vertices: i32,
    _edges: i32,
}
impl RandomUtil {
    fn random_vertex(
        &self,
        rng: &mut dyn RngCore,
    ) -> i32 {
        rng.gen_range(1..=self.vertices)
    }
    fn random_path(
        &self,
        rng: &mut dyn RngCore,
    ) -> (i32, i32) {
        let start = self.random_vertex(rng);
        let mut end = self.random_vertex(rng);

        // Ensure start and end are different
        while end == start {
            end = self.random_vertex(rng);
        }
        (start, end)
    }
}
pub(crate) struct UsersQueriesRepository {
    queries_repository: QueriesRepository,
}

impl Queries for UsersQueriesRepository {
    fn random_query(&mut self) -> Option<(String, QueryType, Query)> {
        self.queries_repository.random_query()
    }
}

impl UsersQueriesRepository {
    pub fn new(
        vertices: i32,
        edges: i32,
    ) -> UsersQueriesRepository {
        let queries_repository = QueriesRepositoryBuilder::new(vertices, edges)
            .flavour(Flavour::FalkorDB)
            .add_query("single_vertex_read", QueryType::Read, |random, _flavour, rng| {
                QueryBuilder::new()
                    .text("MATCH (n:User {id : $id}) RETURN n")
                    .param("id", random.random_vertex(rng))
                    .build()
            })
            .add_query("single_vertex_write", QueryType::Write, |random, _flavour, rng| {
                QueryBuilder::new()
                    .text("CREATE (n:UserTemp {id : $id}) RETURN n")
                    .param("id", random.random_vertex(rng))
                    .build()
            })
            .add_query("single_edge_write", QueryType::Write, |random, _flavour, rng| {
                let (from, to) = random.random_path(rng);
                QueryBuilder::new()
                    .text("MATCH (n:User {id: $from}), (m:User {id: $to}) WITH n, m CREATE (n)-[e:Temp]->(m) RETURN e")
                    .param("from", from)
                    .param("to", to)
                    .build()
            })
            .add_query("aggregate_expansion_1", QueryType::Read, |random, _flavour, rng| {
                QueryBuilder::new()
                    .text("MATCH (s:User {id: $id})-->(n:User) RETURN n.id")
                    .param("id", random.random_vertex(rng))
                    .build()
            })
            .add_query(
                "aggregate_expansion_1_with_filter",
                QueryType::Read,
                |random, _flavour, rng| {
                    QueryBuilder::new()
                        .text("MATCH (s:User {id: $id})-->(n:User)  WHERE n.age >= 18  RETURN n.id")
                        .param("id", random.random_vertex(rng))
                        .build()
                },
            )
            .add_query("aggregate_expansion_2", QueryType::Read, |random, _flavour, rng| {
                QueryBuilder::new()
                    .text("MATCH (s:User {id: $id})-->()-->(n:User) RETURN DISTINCT n.id")
                    .param("id", random.random_vertex(rng))
                    .build()
            })
            .add_query(
                "aggregate_expansion_2_with_filter",
                QueryType::Read,
                |random, _flavour, rng| {
                    QueryBuilder::new()
                        .text("MATCH (s:User {id: $id})-->()-->(n:User)  WHERE n.age >= 18  RETURN DISTINCT n.id")
                        .param("id", random.random_vertex(rng))
                        .build()
                },
            )
            .add_query(
                "aggregate_expansion_3",
                QueryType::Read,
                |random, _flavour, rng| {
                    QueryBuilder::new()
                        .text("MATCH (s:User {id: $id})-->()-->()-->(n:User) RETURN DISTINCT n.id")
                        .param("id", random.random_vertex(rng))
                        .build()
                },
            )
            .add_query(
                "aggregate_expansion_3_with_filter",
                QueryType::Read,
                |random, _flavour, rng| {
                    QueryBuilder::new()
                        .text("MATCH (s:User {id: $id})-->()-->()-->(n:User)  WHERE n.age >= 18  RETURN DISTINCT n.id")
                        .param("id", random.random_vertex(rng))
                        .build()
                },
            )
            .add_query(
                "aggregate_expansion_4",
                QueryType::Read,
                |random, _flavour, rng| {
                    QueryBuilder::new()
                        .text("MATCH (s:User {id: $id})-->()-->()-->()-->(n:User) RETURN DISTINCT n.id")
                        .param("id", random.random_vertex(rng))
                        .build()
                },
            )
            .add_query(
                "aggregate_expansion_4_with_filter",
                QueryType::Read,
                |random, _flavour, rng| {
                    QueryBuilder::new()
                        .text("MATCH (s:User {id: $id})-->()-->()-->()-->(n:User)  WHERE n.age >= 18 RETURN DISTINCT n.id")
                        .param("id", random.random_vertex(rng))
                        .build()
                },
            )
            .build();

        UsersQueriesRepository { queries_repository }
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

        let seed: u64 = 42;
        let mut rng: Lcg64Xsh32 = Pcg32::seed_from_u64(seed);
        let query = generator.generate(&mut rng);
        assert_eq!(query.text, "MATCH (p:Person) RETURN p");
    }
}
