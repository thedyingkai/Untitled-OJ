FROM golang:1.26.3

WORKDIR /app

COPY auth/go.mod auth/go.sum ./auth/
COPY shared/go.mod shared/go.sum ./shared/

WORKDIR /app/auth
RUN go mod download

WORKDIR /app

COPY auth ./auth
COPY shared ./shared

WORKDIR /app/auth

RUN go build -o auth .

CMD ["./auth"]