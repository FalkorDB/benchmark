use crate::query::{Query, QueryBuilder};
use rand::seq::SliceRandom;
use rand::{thread_rng, Rng};
use std::collections::HashMap;

pub(crate) trait Queries {
    fn random_query(&self) -> Option<Query>;
    fn random_queries(
        &self,
        count: i32,
    ) -> Vec<Query> {
        (0..count)
            .map(|_| self.random_query())
            .filter_map(|query| query)
            .collect()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum QueryType {
    Read,
    Write,
}

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

pub struct QueriesRepositoryBuilder {
    vertices: i32,
    edges: i32,
    queries: Vec<(String, QueryType, Box<dyn Fn() -> Query>)>,
}

impl QueriesRepositoryBuilder {
    pub fn new(
        vertices: i32,
        edges: i32,
    ) -> Self {
        QueriesRepositoryBuilder {
            vertices,
            edges,
            queries: Vec::new(),
        }
    }

    pub fn add_query<F>(
        mut self,
        name: impl Into<String>,
        query_type: QueryType,
        generator: F,
    ) -> Self
    where
        F: Fn(&RandomUtil) -> Query + 'static,
    {
        let vertices = self.vertices;
        let edges = self.edges;
        self.queries.push((
            name.into(),
            query_type,
            Box::new(move || {
                let random = RandomUtil { vertices, edges };
                generator(&random)
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

    fn get(
        &self,
        name: &str,
    ) -> Option<&QueryGenerator> {
        self.queries.get(name)
    }

    fn get_all_names(&self) -> Vec<&String> {
        self.queries.keys().collect()
    }
    pub fn all_read(&self) -> QueryTypeView {
        QueryTypeView {
            repo: self,
            query_type: QueryType::Read,
        }
    }
    pub fn all_write(&self) -> QueryTypeView {
        QueryTypeView {
            repo: self,
            query_type: QueryType::Write,
        }
    }
    pub fn mixed(
        &self,
        write_percentage: f64,
    ) -> MixedQueryView {
        MixedQueryView {
            repo: self,
            write_percentage,
        }
    }
    fn random(&self) -> Option<&QueryGenerator> {
        let keys: Vec<&String> = self.queries.keys().collect();
        keys.choose(&mut thread_rng())
            .and_then(|&key| self.queries.get(key))
    }
}

impl Queries for QueriesRepository {
    fn random_query(&self) -> Option<Query> {
        self.random().map(|generator| generator.generate())
    }
}

pub struct QueryTypeView<'a> {
    repo: &'a QueriesRepository,
    query_type: QueryType,
}
impl Queries for QueryTypeView<'_> {
    fn random_query(&self) -> Option<Query> {
        self.random().map(|generator| generator.generate())
    }
}
impl<'a> QueryTypeView<'a> {
    pub fn get(
        &self,
        name: &str,
    ) -> Option<&'a QueryGenerator> {
        self.repo.queries.get(name).and_then(|generator| {
            if generator.query_type == self.query_type {
                Some(generator)
            } else {
                None
            }
        })
    }

    pub fn get_all_names(&self) -> Vec<&'a String> {
        self.repo
            .queries
            .iter()
            .filter(|(_, generator)| generator.query_type == self.query_type)
            .map(|(name, _)| name)
            .collect()
    }

    pub fn random(&self) -> Option<&'a QueryGenerator> {
        let filtered_keys: Vec<&String> = self
            .repo
            .queries
            .iter()
            .filter(|(_, generator)| generator.query_type == self.query_type)
            .map(|(name, _)| name)
            .collect();

        filtered_keys
            .choose(&mut thread_rng())
            .and_then(|&key| self.get(key))
    }
}

pub struct MixedQueryView<'a> {
    repo: &'a QueriesRepository,
    write_percentage: f64,
}

impl Queries for MixedQueryView<'_> {
    fn random_query(&self) -> Option<Query> {
        self.random().map(|generator| generator.generate())
    }
}

impl<'a> MixedQueryView<'a> {
    pub fn random(&self) -> Option<&'a QueryGenerator> {
        let mut rng = thread_rng();
        let is_write = rng.gen_bool(self.write_percentage);

        let filtered_keys: Vec<&String> = self
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
            .map(|(name, _)| name)
            .collect();

        filtered_keys
            .choose(&mut rng)
            .and_then(|&key| self.repo.queries.get(key))
    }

    pub fn get_all_names(&self) -> Vec<&'a String> {
        self.repo.queries.keys().collect()
    }
}

struct RandomUtil {
    vertices: i32,
    edges: i32,
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

pub struct UsersQueriesRepositoryBuilder {
    vertices: i32,
    edges: i32,
    queries: Vec<(String, QueryType, Box<dyn Fn() -> Query>)>,
}

pub(crate) struct UsersQueriesRepository {
    queries_repository: QueriesRepository,
}

impl Queries for UsersQueriesRepository {
    fn random_query(&self) -> Option<Query> {
        self.queries_repository.random_query()
    }
}

impl UsersQueriesRepository {
    pub fn new(
        vertices: i32,
        edges: i32,
    ) -> UsersQueriesRepository {
        let mut queries_repository = QueriesRepositoryBuilder::new(vertices, edges)
            .add_query("single_vertex_read", QueryType::Read, |random| {
                QueryBuilder::new()
                    .text("MATCH (n:User {id : $id}) RETURN n")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("single_vertex_write", QueryType::Write, |random| {
                QueryBuilder::new()
                    .text("CREATE (n:UserTemp {id : $id}) RETURN n")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("single_edge_write", QueryType::Write, |random| {
                let (from, to) = random.random_path();
                QueryBuilder::new()
                    .text( "MATCH (n:User {id: $from}), (m:User {id: $to}) WITH n, m CREATE (n)-[e:Temp]->(m) RETURN e")
                    .param("from", from)
                    .param("to", to)
                    .build()
            })
            .add_query("aggregate", QueryType::Read, |random| {
                QueryBuilder::new()
                    .text("MATCH (n:User) RETURN n.age, COUNT(*)")
                    .build()
            })
            .add_query("aggregate_distinct", QueryType::Read, |random| {
                QueryBuilder::new()
                    .text("MATCH (n:User) RETURN COUNT(DISTINCT n.age)")
                    .build()
            })
            .add_query("aggregate_with_filter", QueryType::Read, |random| {
                QueryBuilder::new()
                    .text("MATCH (n:User) WHERE n.age >= 18 RETURN n.age, COUNT(*)")
                    .build()
            })
            .add_query("aggregate_expansion_1", QueryType::Read, |random| {
                QueryBuilder::new()
                    .text("MATCH (s:User {id: $id})-->(n:User) RETURN n.id")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query(
                "aggregate_expansion_1_with_filter",
                QueryType::Read,
                |random| {
                    QueryBuilder::new()
                        .text("MATCH (s:User {id: $id})-->(n:User)  WHERE n.age >= 18  RETURN n.id")
                        .param("id", random.random_vertex())
                        .build()
                },
            )
            .add_query("aggregate_expansion_2", QueryType::Read, |random| {
                QueryBuilder::new()
                    .text("MATCH (s:User {id: $id})-->()-->(n:User) RETURN DISTINCT n.id")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query(
                "aggregate_expansion_2_with_filter",
                QueryType::Read,
                |random| {
                    QueryBuilder::new()
                        .text("MATCH (s:User {id: $id})-->()-->(n:User)  WHERE n.age >= 18  RETURN DISTINCT n.id")
                        .param("id", random.random_vertex())
                        .build()
                },
            )
            .add_query(
                "aggregate_expansion_3",
                QueryType::Read,
                |random| {
                    QueryBuilder::new()
                        .text("MATCH (s:User {id: $id})-->()-->()-->(n:User) RETURN DISTINCT n.id")
                        .param("id", random.random_vertex())
                        .build()
                },
            )
            .add_query(
                "aggregate_expansion_3_with_filter",
                QueryType::Read,
                |random| {
                    QueryBuilder::new()
                        .text("MATCH (s:User {id: $id})-->()-->()-->(n:User)  WHERE n.age >= 18  RETURN DISTINCT n.id",)
                        .param("id", random.random_vertex())
                        .build()
                },
            )
            .add_query(
                "aggregate_expansion_4",
                QueryType::Read,
                |random| {
                    QueryBuilder::new()
                        .text("MATCH (s:User {id: $id})-->()-->()-->()-->(n:User) RETURN DISTINCT n.id")
                        .param("id", random.random_vertex())
                        .build()
                },
            )
            .add_query(
                "aggregate_expansion_4_with_filter",
                QueryType::Read,
                |random| {
                    QueryBuilder::new()
                        .text("MATCH (s:User {id: $id})-->()-->()-->()-->(n:User)  WHERE n.age >= 18 RETURN DISTINCT n.id")
                        .param("id", random.random_vertex())
                        .build()
                },
            )
            .build();

        UsersQueriesRepository { queries_repository }
    }

    pub fn get_query(
        &self,
        name: &str,
    ) -> Option<&QueryGenerator> {
        self.queries_repository.get(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::QueryParam;

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
    fn test_queries_repository_add_and_get() {
        let mut repo = QueriesRepository::new();
        repo.add("test_query", QueryType::Read, || {
            QueryBuilder::new()
                .text("MATCH (p:Person) RETURN p")
                .build()
        });

        let generator = repo.get("test_query").unwrap();
        let query = generator.generate();
        assert_eq!(query.text, "MATCH (p:Person) RETURN p");
    }

    #[test]
    fn test_queries_repository_get_all_names() {
        let mut repo = QueriesRepository::new();
        repo.add("query1", QueryType::Read, || QueryBuilder::new().build());
        repo.add("query2", QueryType::Write, || QueryBuilder::new().build());

        let names = repo.get_all_names();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&&"query1".to_string()));
        assert!(names.contains(&&"query2".to_string()));
    }

    #[test]
    fn test_queries_repository_random() {
        let mut repo = QueriesRepository::new();
        repo.add("query1", QueryType::Read, || QueryBuilder::new().build());
        repo.add("query2", QueryType::Write, || QueryBuilder::new().build());

        let random_generator = repo.random();
        assert!(random_generator.is_some());
    }

    #[test]
    fn test_query_modification() {
        let mut repo = QueriesRepository::new();
        repo.add("find_person", QueryType::Read, || {
            QueryBuilder::new()
                .text("MATCH (p:Person {name: $name, birth_year: $birth_year}) RETURN p")
                .param("name", "Niccolò Machiavelli")
                .param("birth_year", 1469)
                .build()
        });

        let generator = repo.get("find_person").unwrap();
        let original_query = generator.generate();

        let modified_query = QueryBuilder::new()
            .text(&original_query.text)
            .param("name", "Leonardo da Vinci")
            .param("birth_year", 1452)
            .build();

        assert_eq!(modified_query.text, original_query.text);
        assert_eq!(
            modified_query.params.get("name"),
            Some(&QueryParam::String("Leonardo da Vinci".to_string()))
        );
        assert_eq!(
            modified_query.params.get("birth_year"),
            Some(&QueryParam::Integer(1452))
        );
    }
    #[test]
    fn test_queries_repository_all_read() {
        let mut repo = QueriesRepository::new();
        repo.add("read1", QueryType::Read, || {
            QueryBuilder::new().text("READ1").build()
        });
        repo.add("read2", QueryType::Read, || {
            QueryBuilder::new().text("READ2").build()
        });
        repo.add("write1", QueryType::Write, || {
            QueryBuilder::new().text("WRITE1").build()
        });

        let read_view = repo.all_read();
        let names = read_view.get_all_names();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&&"read1".to_string()));
        assert!(names.contains(&&"read2".to_string()));
        assert!(!names.contains(&&"write1".to_string()));

        assert!(read_view.get("read1").is_some());
        assert!(read_view.get("write1").is_none());
    }

    #[test]
    fn test_queries_repository_all_write() {
        let mut repo = QueriesRepository::new();
        repo.add("read1", QueryType::Read, || {
            QueryBuilder::new().text("READ1").build()
        });
        repo.add("write1", QueryType::Write, || {
            QueryBuilder::new().text("WRITE1").build()
        });
        repo.add("write2", QueryType::Write, || {
            QueryBuilder::new().text("WRITE2").build()
        });

        let write_view = repo.all_write();
        let names = write_view.get_all_names();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&&"write1".to_string()));
        assert!(names.contains(&&"write2".to_string()));
        assert!(!names.contains(&&"read1".to_string()));

        assert!(write_view.get("write1").is_some());
        assert!(write_view.get("read1").is_none());
    }

    #[test]
    fn test_queries_repository_random_filtered() {
        let mut repo = QueriesRepository::new();
        repo.add("read1", QueryType::Read, || {
            QueryBuilder::new().text("READ1").build()
        });
        repo.add("read2", QueryType::Read, || {
            QueryBuilder::new().text("READ2").build()
        });
        repo.add("write1", QueryType::Write, || {
            QueryBuilder::new().text("WRITE1").build()
        });

        let read_view = repo.all_read();
        let random_read = read_view.random();
        assert!(random_read.is_some());
        assert!(matches!(
            random_read.unwrap().generate().text.as_str(),
            "READ1" | "READ2"
        ));

        let write_view = repo.all_write();
        let random_write = write_view.random();
        assert!(random_write.is_some());
        assert_eq!(random_write.unwrap().generate().text, "WRITE1");
    }
    #[test]
    fn test_queries_repository_mixed() {
        let mut repo = QueriesRepository::new();
        repo.add("read1", QueryType::Read, || {
            QueryBuilder::new().text("READ1").build()
        });
        repo.add("read2", QueryType::Read, || {
            QueryBuilder::new().text("READ2").build()
        });
        repo.add("write1", QueryType::Write, || {
            QueryBuilder::new().text("WRITE1").build()
        });
        repo.add("write2", QueryType::Write, || {
            QueryBuilder::new().text("WRITE2").build()
        });

        let mixed_view = repo.mixed(0.7); // 70% write, 30% read

        // Test multiple times to ensure both read and write queries are selected
        let mut write_count = 0;
        let mut read_count = 0;
        for _ in 0..1000 {
            let random_query = mixed_view.random().unwrap();
            match random_query.query_type {
                QueryType::Write => write_count += 1,
                QueryType::Read => read_count += 1,
            }
        }

        assert!(write_count > read_count); // Ensure write queries are selected more often
        assert!(read_count > 0); // Ensure read queries are also selected
    }
}
