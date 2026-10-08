"""Run inside a Sysbox worker: real published-port and cleanup coverage."""
import docker
import psycopg2
from testcontainers.postgres import PostgresContainer

client = docker.from_env()
with PostgresContainer("postgres:16-alpine") as postgres:
    container_id = postgres.get_wrapped_container().id
    with psycopg2.connect(
        host=postgres.get_container_host_ip(),
        port=postgres.get_exposed_port(5432),
        user=postgres.username,
        password=postgres.password,
        dbname=postgres.dbname,
    ) as connection:
        with connection.cursor() as cursor:
            cursor.execute("SELECT 42")
            assert cursor.fetchone() == (42,)
try:
    client.containers.get(container_id)
except docker.errors.NotFound:
    print("Testcontainers database, published port, and cleanup passed")
else:
    raise AssertionError("Testcontainers left the database container behind")
