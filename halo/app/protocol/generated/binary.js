// Generated from protocol/openapi.json. Do not edit.
export default {
  "POST /api/v1/workforce/application-photos": {
    "responses": {},
    "input": [
      "image/jpeg",
      "image/png",
      "image/webp"
    ]
  },
  "GET /api/v1/workforce/application-resources/{id}": {
    "responses": {
      "200": [
        "image/webp",
        "font/ttf",
        "font/otf"
      ]
    }
  },
  "GET /api/v1/workforce/badges/{image}": {
    "responses": {
      "200": [
        "image/webp"
      ]
    }
  }
};
