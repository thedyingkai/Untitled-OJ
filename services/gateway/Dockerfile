FROM golang:1.26.3

WORKDIR /app

COPY gateway/go.mod gateway/go.sum ./gateway/
COPY shared/go.mod shared/go.sum ./shared/

WORKDIR /app/gateway
RUN go mod download

WORKDIR /app
COPY gateway ./gateway
COPY shared ./shared

WORKDIR /app/gateway
RUN go build -o gateway .

CMD ["./gateway"]