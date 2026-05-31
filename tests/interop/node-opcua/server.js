// Minimal node-opcua sample server for cross-implementation interoperability
// testing of `opcua-scan`.
//
// It exposes the standard OPC UA address space (including
// Server/ServerStatus/CurrentTime) plus a small custom namespace with a few
// variables of different data types, so browse/read/subscribe paths can be
// exercised against a Node.js server implementation.

const {
  OPCUAServer,
  Variant,
  DataType,
  StatusCodes,
} = require("node-opcua");

const port = parseInt(process.env.OPCUA_PORT || "4841", 10);

(async () => {
  const server = new OPCUAServer({
    port,
    resourcePath: "/",
    buildInfo: {
      productName: "opcua-scan-interop-node-opcua",
      buildNumber: "1",
      buildDate: new Date(),
    },
    // Anonymous access over an unsecured endpoint keeps the interop checks
    // simple; this server is for testing only.
    securityPolicies: undefined,
  });

  await server.initialize();

  const addressSpace = server.engine.addressSpace;
  const namespace = addressSpace.registerNamespace("urn:opcua-scan-interop");

  const factory = namespace.addObject({
    organizedBy: addressSpace.rootFolder.objects,
    browseName: "Factory",
  });

  let temperature = 25.0;
  namespace.addVariable({
    componentOf: factory,
    browseName: "Temperature",
    dataType: "Double",
    value: {
      get: () => new Variant({ dataType: DataType.Double, value: temperature }),
    },
  });

  namespace.addVariable({
    componentOf: factory,
    browseName: "Running",
    dataType: "Boolean",
    value: { get: () => new Variant({ dataType: DataType.Boolean, value: true }) },
  });

  namespace.addVariable({
    componentOf: factory,
    browseName: "Label",
    dataType: "String",
    value: { get: () => new Variant({ dataType: DataType.String, value: "M-100" }) },
  });

  // Drive a value change so subscription tests observe updates.
  setInterval(() => {
    temperature = 20 + Math.random() * 10;
  }, 1000);

  await server.start();
  const endpoint = server.endpoints[0].endpointDescriptions()[0].endpointUrl;
  // eslint-disable-next-line no-console
  console.log(`node-opcua interop server listening at ${endpoint}`);

  process.on("SIGTERM", async () => {
    await server.shutdown();
    process.exit(0);
  });
})().catch((err) => {
  // eslint-disable-next-line no-console
  console.error(err);
  process.exit(1);
});
