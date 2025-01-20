#!/bin/bash

# Determine the operating system
OS="$(uname)"

# Define the Docker Compose file content
if [[ "$OS" == "Darwin" ]]; then
    # macOS configuration
    cat <<EOF > docker-compose.yml
services:
  prometheus:
    image: prom/prometheus:latest
    extra_hosts:
      - "host.docker.internal:host-gateway"
    ports:
      - 9090:9090
    volumes:
      - ./prometheus.yml:/etc/prometheus/prometheus.yml
    command:
      - '--config.file=/etc/prometheus/prometheus.yml'

  redis-exporter:
    image: oliver006/redis_exporter:latest
    command:
      - '--redis.addr=host.docker.internal:6379'
    ports:
      - 9121:9121
    extra_hosts:
      - "host.docker.internal:host-gateway"

  grafana:
    image: grafana/grafana:latest
    ports:
      - 3000:3000
    environment:
      - GF_AUTH_DISABLE_LOGIN_FORM=true
      - GF_AUTH_ANONYMOUS_ENABLED=true
      - GF_AUTH_ANONYMOUS_ORG_ROLE=Admin
      - GF_DASHBOARDS_JSON_ENABLED=true
    volumes:
      - ./grafana-datasources.yml:/etc/grafana/provisioning/datasources/datasources.yml
      - ./dashboards:/var/lib/grafana/dashboards
      - ./provisioning:/etc/grafana/provisioning

volumes:
  prometheus_data:
EOF

cat <<EOF > prometheus.yml
global:
  scrape_interval: 5s
  scrape_timeout: 500ms

scrape_configs:

  - job_name: 'falkor_benchmark'
    static_configs:
      - targets: [ 'host.docker.internal:8080' ]

  - job_name: 'redis'
    static_configs:
      - targets: [ 'redis-exporter:9121' ]
EOF

cat <<EOF > grafana-datasources.yml
apiVersion: 1

datasources:
  - name: Prometheus
    type: prometheus
    access: proxy
    url: http://prometheus:9090
    isDefault: true
EOF

elif [[ "$OS" == "Linux" ]]; then
    # Linux configuration
    HOST_IP=$(hostname -I | awk '{print $1}')  # Get the host IP address

    cat <<EOF > docker-compose.yml
version: '3.8'

services:
  prometheus:
    image: prom/prometheus:latest
    ports:
      - 9090:9090
    volumes:
      - ./prometheus.yml:/etc/prometheus/prometheus.yml
    command:
      - '--config.file=/etc/prometheus/prometheus.yml'

  redis-exporter:
    image: oliver006/redis_exporter:latest
    command:
      - '--redis.addr=${HOST_IP}:6379'  # Use host IP for Linux
    ports:
      - 9121:9121

  grafana:
    image: grafana/grafana:latest
    ports:
      - 3000:3000
    environment:
      - GF_AUTH_DISABLE_LOGIN_FORM=true
      - GF_AUTH_ANONYMOUS_ENABLED=true
      - GF_AUTH_ANONYMOUS_ORG_ROLE=Admin
      - GF_DASHBOARDS_JSON_ENABLED=true
    volumes:
      - ./grafana-datasources.yml:/etc/grafana/provisioning/datasources/datasources.yml
      - ./dashboards:/var/lib/grafana/dashboards
      - ./provisioning:/etc/grafana/provisioning

  node-exporter:
      image: prom/node-exporter:latest
      ports:
        - 9100:9100

volumes:
  prometheus_data:
EOF

cat <<EOF > prometheus.yml
global:
  scrape_interval: 5s
  scrape_timeout: 500ms

scrape_configs:

  - job_name: 'falkor_benchmark'
    static_configs:
      - targets: [ '${HOST_IP}:8080' ] # Use host IP for Linux

  - job_name: 'redis'
    static_configs:
      - targets: [ 'redis-exporter:9121' ]

  - job_name: 'node-exporter'
    static_configs:
      - targets: [ 'node-exporter:9100' ]

EOF

cat <<EOF > grafana-datasources.yml
apiVersion: 1

datasources:
  - name: Prometheus
    type: prometheus
    access: proxy
    url: http://prometheus:9090
    isDefault: true
EOF

else
    echo "Unsupported OS"
    exit 1
fi

echo "Docker Compose files generated successfully."
