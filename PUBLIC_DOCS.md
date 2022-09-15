## Endpoints

Return the Mode of the Light Client

```bash
curl -s localhost:7000/v1/mode
```

Returns the latest block 

```bash
curl -s localhost:7000/v1/latest_block
```

Given a block number (as _(hexa-)_ decimal number), return confidence obtained by the light client for this block:

```bash
curl -s localhost:7000/v1/confidence/ <block-number>
```

Result:

```json
{
    "number": 223,
    "confidence": 99.90234375,
    "serialisedConfidence": "958776730446"
}
```

>  `serialisedConfidence` is calculated as: 
> `blockNumber << 32 | int32(confidence * 10 ** 7)`, where confidence is represented out of 10 ** 9.


Given a block number (as _(hexa-)_ decimal number), return the content of the data if specified in config in hex string format

```bash
curl -s localhost:7000/v1/appdata/ <block-number>
```

Result:

```json
{"block":386,"extrinsics":[{"app_id":1,"signature":{"Sr25519":"be86221cc07a461537570637d75a0569c2210286e85c693e3b31d94211b1ef1eaf451b13072066f745f70801ad6af0dcdf2e42b7bf77be2dc6709196b4d45889"},"data":"0x313537626233643536653339356537393237633664"}]}
```

Return the status of a latest block 

```bash
curl -s localhost:7000/v1/status
```
