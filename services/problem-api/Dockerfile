FROM golang:1.26.3

WORKDIR /app

COPY problem-api/go.mod problem-api/go.sum ./problem-api/
COPY shared/go.mod shared/go.sum ./shared/

WORKDIR /app/problem-api
RUN go mod download

WORKDIR /app

COPY problem-api ./problem-api
COPY shared ./shared

WORKDIR /app/problem-api

RUN go build -o problem-api .

RUN mkdir -p /data/ojos/problems

CMD ["./problem-api", "-f", "etc/problemapi.yaml"]