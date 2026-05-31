FROM golang:1.26.3

WORKDIR /app

COPY judge-api/go.mod judge-api/go.sum ./judge-api/
COPY shared/go.mod shared/go.sum ./shared/

WORKDIR /app/judge-api
RUN go mod download

WORKDIR /app

COPY judge-api ./judge-api
COPY shared ./shared

WORKDIR /app/judge-api

RUN go build -o judge-api .

CMD ["./judge-api", "-f", "etc/judgeapi.yaml"]