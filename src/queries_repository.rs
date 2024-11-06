use crate::query::{Query, QueryBuilder};
use rand::seq::SliceRandom;
use rand::{thread_rng, Rng};
use std::collections::HashMap;

pub(crate) trait Queries {
    fn random_query(&self) -> Option<(String, QueryType, Query)>;

    fn random_queries(
        &self,
        count: i32,
    ) -> Box<dyn Iterator<Item = (String, QueryType, Query)> + '_> {
        Box::new((0..count).filter_map(move |_| self.random_query()))
    }
    // fn random_queries(
    //     &self,
    //     count: i32,
    // ) -> Vec<(String, QueryType, Query)> {
    //     (0..count)
    //         .map(|_| self.random_query())
    //         .filter_map(|query| query)
    //         .collect()
    // }
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
    generator: Box<dyn Fn() -> Query>,
}

impl QueryGenerator {
    pub fn new<F>(
        query_type: QueryType,
        generator: F,
    ) -> Self
    where
        F: Fn() -> Query + 'static,
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

pub struct QueriesRepositoryBuilder<U> {
    vertices: i32,
    edges: i32,
    queries: Vec<(String, QueryType, Box<dyn Fn() -> Query>)>,
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
        F: Fn(&RandomUtil, Flavour) -> Query + 'static,
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
    queries: HashMap<String, QueryGenerator>,
}

impl QueriesRepository {
    fn new() -> Self {
        QueriesRepository {
            queries: HashMap::new(),
        }
    }

    fn add<F>(
        &mut self,
        name: impl Into<String>,
        query_type: QueryType,
        generator: F,
    ) where
        F: Fn() -> Query + 'static,
    {
        self.queries
            .insert(name.into(), QueryGenerator::new(query_type, generator));
    }

    #[allow(dead_code)]
    pub fn all_read(&self) -> QueryTypeView {
        QueryTypeView {
            repo: self,
            query_type: QueryType::Read,
        }
    }
    #[allow(dead_code)]
    pub fn all_write(&self) -> QueryTypeView {
        QueryTypeView {
            repo: self,
            query_type: QueryType::Write,
        }
    }
    #[allow(dead_code)]
    pub fn mixed(
        &self,
        write_percentage: f64,
    ) -> MixedQueryView {
        MixedQueryView {
            repo: self,
            write_percentage,
        }
    }
    fn choose_random_query<'a, I>(
        &'a self,
        keys: I,
    ) -> Option<(String, QueryType, Query)>
    where
        I: IntoIterator<Item = &'a String>,
    {
        keys.into_iter()
            .collect::<Vec<_>>()
            .choose(&mut thread_rng())
            .and_then(|&key| self.queries.get(key).map(|generator| (key, generator)))
            .map(|(name, generator)| (name.to_string(), generator.query_type, generator.generate()))
    }
}

impl Queries for QueriesRepository {
    fn random_query(&self) -> Option<(String, QueryType, Query)> {
        let keys: Vec<&String> = self.queries.keys().collect();
        keys.choose(&mut thread_rng()).map(|&key| {
            let generator = self.queries.get(key).unwrap();
            (key.clone(), generator.query_type, generator.generate())
        })
    }
}

pub struct QueryTypeView<'a> {
    repo: &'a QueriesRepository,
    query_type: QueryType,
}
impl Queries for QueryTypeView<'_> {
    fn random_query(&self) -> Option<(String, QueryType, Query)> {
        let filtered_keys = self
            .repo
            .queries
            .iter()
            .filter(|(_, generator)| generator.query_type == self.query_type)
            .map(|(name, _)| name);

        self.repo.choose_random_query(filtered_keys)
    }
}

pub struct MixedQueryView<'a> {
    repo: &'a QueriesRepository,
    write_percentage: f64,
}

impl Queries for MixedQueryView<'_> {
    fn random_query(&self) -> Option<(String, QueryType, Query)> {
        let mut rng = thread_rng();
        let is_write = rng.gen_bool(self.write_percentage);
        let filtered_keys = self
            .repo
            .queries
            .iter()
            .filter(|(_, generator)| {
                if is_write {
                    generator.query_type == QueryType::Write
                } else {
                    generator.query_type == QueryType::Read
                }
            })
            .map(|(name, _)| name);

        self.repo.choose_random_query(filtered_keys)
    }
}

struct RandomUtil {
    vertices: i32,
    _edges: i32,
}
impl RandomUtil {
    fn random_vertex(&self) -> i32 {
        let mut rng = thread_rng();
        rng.gen_range(1..=self.vertices)
    }
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
pub(crate) struct UsersQueriesRepository {
    queries_repository: QueriesRepository,
}

impl Queries for UsersQueriesRepository {
    fn random_query(&self) -> Option<(String, QueryType, Query)> {
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
            .add_query("single_vertex_read", QueryType::Read, |random,  _flavour| {
                QueryBuilder::new()
                    .text("MATCH (n:User {id : $id}) RETURN n")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("single_vertex_write", QueryType::Write, |random,  _flavour| {
                QueryBuilder::new()
                    .text("CREATE (n:UserTemp {id : $id}) RETURN n")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("single_edge_write", QueryType::Write, |random,  _flavour| {
                let (from, to) = random.random_path();
                QueryBuilder::new()
                    .text("MATCH (n:User {id: $from}), (m:User {id: $to}) WITH n, m CREATE (n)-[e:Temp]->(m) RETURN e")
                    .param("from", from)
                    .param("to", to)
                    .build()
            })
            .add_query("aggregate", QueryType::Read, |_random,  _flavour| {
                QueryBuilder::new()
                    .text("MATCH (n:User) RETURN n.age, COUNT(*)")
                    .build()
            })
            .add_query("aggregate_distinct", QueryType::Read, |_random,  _flavour| {
                QueryBuilder::new()
                    .text("MATCH (n:User) RETURN COUNT(DISTINCT n.age)")
                    .build()
            })
            .add_query("aggregate_with_filter", QueryType::Read, |_random,  _flavour| {
                QueryBuilder::new()
                    .text("MATCH (n:User) WHERE n.age >= 18 RETURN n.age, COUNT(*)")
                    .build()
            })
            .add_query("aggregate_expansion_1", QueryType::Read, |random,  _flavour|{
                QueryBuilder::new()
                    .text("MATCH (s:User {id: $id})-->(n:User) RETURN n.id")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query(
                "aggregate_expansion_1_with_filter",
                QueryType::Read,
                |random,  _flavour| {
                    QueryBuilder::new()
                        .text("MATCH (s:User {id: $id})-->(n:User)  WHERE n.age >= 18  RETURN n.id")
                        .param("id", random.random_vertex())
                        .build()
                },
            )
            .add_query("aggregate_expansion_2", QueryType::Read, |random,  _flavour| {
                QueryBuilder::new()
                    .text("MATCH (s:User {id: $id})-->()-->(n:User) RETURN DISTINCT n.id")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query(
                "aggregate_expansion_2_with_filter",
                QueryType::Read,
                |random,  _flavour| {
                    QueryBuilder::new()
                        .text("MATCH (s:User {id: $id})-->()-->(n:User)  WHERE n.age >= 18  RETURN DISTINCT n.id")
                        .param("id", random.random_vertex())
                        .build()
                },
            )
            .add_query(
                "aggregate_expansion_3",
                QueryType::Read,
                |random,  _flavour| {
                    QueryBuilder::new()
                        .text("MATCH (s:User {id: $id})-->()-->()-->(n:User) RETURN DISTINCT n.id")
                        .param("id", random.random_vertex())
                        .build()
                },
            )
            .add_query(
                "aggregate_expansion_3_with_filter",
                QueryType::Read,
                |random,  _flavour| {
                    QueryBuilder::new()
                        .text("MATCH (s:User {id: $id})-->()-->()-->(n:User)  WHERE n.age >= 18  RETURN DISTINCT n.id",)
                        .param("id", random.random_vertex())
                        .build()
                },
            )
            .add_query(
                "aggregate_expansion_4",
                QueryType::Read,
                |random,  _flavour|{
                    QueryBuilder::new()
                        .text("MATCH (s:User {id: $id})-->()-->()-->()-->(n:User) RETURN DISTINCT n.id")
                        .param("id", random.random_vertex())
                        .build()
                },
            )
            .add_query(
                "aggregate_expansion_4_with_filter",
                QueryType::Read,
                |random,  _flavour| {
                    QueryBuilder::new()
                        .text("MATCH (s:User {id: $id})-->()-->()-->()-->(n:User)  WHERE n.age >= 18 RETURN DISTINCT n.id")
                        .param("id", random.random_vertex())
                        .build()
                },
            )
            .add_query(
                "neighbours_2",
                QueryType::Read,
                |random,  _flavour|{
                    QueryBuilder::new()
                        .text("MATCH (s:User {id: $id})-[*1..2]->(n:User) RETURN DISTINCT n.id")
                        .param("id", random.random_vertex())
                        .build()
                },
            )
            .add_query(
                "neighbours_2_with_filter",
                QueryType::Read,
                |random,  _flavour|{
                    QueryBuilder::new()
                        .text("MATCH (s:User {id: $id})-[*1..2]->(n:User)  WHERE n.age >= 18  RETURN DISTINCT n.id")
                        .param("id", random.random_vertex())
                        .build()
                },
            )
            .add_query(
                "neighbours_2_with_data",
                QueryType::Read,
                |random,  _flavour| {
                    QueryBuilder::new()
                        .text( "MATCH (s:User {id: $id})-[*1..2]->(n:User) RETURN DISTINCT n.id, n")
                        .param("id", random.random_vertex())
                        .build()
                },
            )
            .add_query(
                "neighbours_2_with_data_and_filter",
                QueryType::Read,
                |random,  _flavour| {
                    QueryBuilder::new()
                        .text( "MATCH (s:User {id: $id})-[*1..2]->(n:User) WHERE n.age >= 18 RETURN DISTINCT n.id, n")
                        .param("id", random.random_vertex())
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

        let query = generator.generate();
        assert_eq!(query.text, "MATCH (p:Person) RETURN p");
    }
}
