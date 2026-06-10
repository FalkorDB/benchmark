# Query Operations Documentation

This document contains the explanations and sample Cypher queries for all 16 operations documented in the benchmark side panel UI under the "Queries" section.

---

## 1. Expand 4L (`aggregate_expansion_4`)
* **Type**: Read
* **Description**: Expands 4 levels deep in the graph from a starting user and returns distinct destination user IDs.
* **Sample Cypher**:
  ```cypher
  MATCH (s:User {id: $id})-->()-->()-->()-->(n:User)
  RETURN DISTINCT n.id
  ```

## 2. Expand 4L (Filtered) (`aggregate_expansion_4_with_filter`)
* **Type**: Read
* **Description**: Expands 4 levels deep in the graph with an age filter (age >= 18) on the destination nodes, returning distinct user IDs.
* **Sample Cypher**:
  ```cypher
  MATCH (s:User {id: $id})-->()-->()-->()-->(n:User)
  WHERE n.age >= 18
  RETURN DISTINCT n.id
  ```

## 3. Expand 3L (`aggregate_expansion_3`)
* **Type**: Read
* **Description**: Expands 3 levels deep in the graph from a starting user and returns distinct destination user IDs.
* **Sample Cypher**:
  ```cypher
  MATCH (s:User {id: $id})-->()-->()-->(n:User)
  RETURN DISTINCT n.id
  ```

## 4. Expand 3L (Filtered) (`aggregate_expansion_3_with_filter`)
* **Type**: Read
* **Description**: Expands 3 levels deep in the graph with an age filter (age >= 18) on the destination nodes, returning distinct user IDs.
* **Sample Cypher**:
  ```cypher
  MATCH (s:User {id: $id})-->()-->()-->(n:User)
  WHERE n.age >= 18
  RETURN DISTINCT n.id
  ```

## 5. Expand 2L (`aggregate_expansion_2`)
* **Type**: Read
* **Description**: Expands 2 levels deep in the graph from a starting user and returns distinct destination user IDs.
* **Sample Cypher**:
  ```cypher
  MATCH (s:User {id: $id})-->()-->(n:User)
  RETURN DISTINCT n.id
  ```

## 6. Expand 2L (Filtered) (`aggregate_expansion_2_with_filter`)
* **Type**: Read
* **Description**: Expands 2 levels deep in the graph with an age filter (age >= 18) on the destination nodes, returning distinct user IDs.
* **Sample Cypher**:
  ```cypher
  MATCH (s:User {id: $id})-->()-->(n:User)
  WHERE n.age >= 18
  RETURN DISTINCT n.id
  ```

## 7. Expand 1L (`aggregate_expansion_1`)
* **Type**: Read
* **Description**: Expands 1 level deep in the graph from a starting user and returns connected user IDs.
* **Sample Cypher**:
  ```cypher
  MATCH (s:User {id: $id})-->(n:User)
  RETURN n.id
  ```

## 8. Expand 1L (Filtered) (`aggregate_expansion_1_with_filter`)
* **Type**: Read
* **Description**: Expands 1 level deep in the graph with an age filter (age >= 18) on connected user nodes, returning connected user IDs.
* **Sample Cypher**:
  ```cypher
  MATCH (s:User {id: $id})-->(n:User)
  WHERE n.age >= 18
  RETURN n.id
  ```

## 9. Aggregate age (Filtered) (`aggregate_age_filtered`)
* **Type**: Read
* **Description**: Calculates the average age of all users aged 18 or older.
* **Sample Cypher**:
  ```cypher
  MATCH (n:User)
  WHERE n.age >= 18
  RETURN avg(n.age) AS avg_age
  ```

## 10. Count users (`aggregate_count_users`)
* **Type**: Read
* **Description**: Retrieves the total count of user nodes. Uses high-performance metadata stats in FalkorDB, and global node count fallback in other vendors.
* **Sample Cypher**:
  ```cypher
  // FalkorDB:
  CALL db.meta.stats() YIELD nodeCount RETURN nodeCount AS cnt

  // Neo4j/Memgraph:
  MATCH (n:User) RETURN count(n) AS cnt
  ```

## 11. Neighbours 2L (data+filter) (`neighbours_2_with_data_and_filter`)
* **Type**: Read
* **Description**: Retrieves 2-hop neighbor nodes (with age >= 18) and returns full properties.
* **Sample Cypher**:
  ```cypher
  MATCH (s:User {id: $id})-->()-->(n:User)
  WHERE n.age >= 18
  RETURN n
  ```

## 12. Shortest path (`shortest_path`)
* **Type**: Read
* **Description**: Finds the shortest path between two users and returns its length.
* **Sample Cypher**:
  ```cypher
  // FalkorDB:
  MATCH (s:User {id: $from}), (t:User {id: $to}) WITH shortestPath((s)-[*]->(t)) AS p RETURN length(p)

  // Neo4j:
  MATCH (s:User {id: $from}), (t:User {id: $to}) MATCH p = shortestPath((s)-[*]->(t)) RETURN length(p)

  // Memgraph:
  MATCH p = (:User {id: $from})-[*BFS]->(:User {id: $to}) RETURN length(p)
  ```

## 13. Write Edge (`single_edge_write`)
* **Type**: Write
* **Description**: Matches two users by ID and creates a `Friend` relationship between them.
* **Sample Cypher**:
  ```cypher
  MATCH (n:User {id: $from}), (m:User {id: $to})
  WITH n, m
  CREATE (n)-[e:Friend]->(m)
  RETURN e
  ```

## 14. Write Vertex (`single_vertex_write`)
* **Type**: Write
* **Description**: Creates a new User vertex with a specific ID.
* **Sample Cypher**:
  ```cypher
  CREATE (n:User {id: $id})
  RETURN n
  ```

## 15. Write General (`write`)
* **Type**: Write
* **Description**: Updates attributes (e.g. social credit score) of a User node.
* **Sample Cypher**:
  ```cypher
  MATCH (n:User {id: $id})
  SET n.rpc_social_credit = $rpc_social_credit
  RETURN n
  ```

## 16. Read Vertex (`single_vertex_read`)
* **Type**: Read
* **Description**: Retrieves a single User vertex by ID.
* **Sample Cypher**:
  ```cypher
  MATCH (n:User {id: $id})
  RETURN n
  ```
