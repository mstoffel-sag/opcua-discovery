FROM node:20-alpine

WORKDIR /app

# Install only the node-opcua runtime. Pinned major version for reproducibility.
RUN npm init -y >/dev/null 2>&1 && npm install node-opcua@2

COPY server.js ./

EXPOSE 4841

CMD ["node", "server.js"]
