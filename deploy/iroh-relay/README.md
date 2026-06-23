# Iroh Relay on Kubernetes

This deploys a single `iroh-relay` instance for Alleycat/Litter pairing.

## Fill these values

The public DNS name must point to the HAProxy/Ingress entrypoint.

## Build and push the image

```powershell
docker build -t ghcr.io/aegean-org/iroh-relay:0.98.0 D:\project\alleycat\deploy\iroh-relay
docker push ghcr.io/aegean-org/iroh-relay:0.98.0
```

The repository also includes `.github/workflows/iroh-relay-image.yml`, which
pushes the same image to GHCR on demand or when relay deployment files change.

## Deploy

```powershell
kubectl apply -f D:\project\alleycat\deploy\iroh-relay\k8s.yaml
kubectl -n necode-relay get svc iroh-relay-public
kubectl -n necode-relay logs deploy/iroh-relay -f
```

The Ingress/HAProxy entrypoint must support WebSocket upgrade and expose:

- TCP `80`
- TCP `443`

This template intentionally disables QUIC address discovery because the current cluster exposes services through HAProxy and Ingress, not a L4 UDP load balancer.

## Verify

After DNS points to the HAProxy/Ingress entrypoint:

```powershell
curl.exe -i http://relay.inoteexpress.com/generate_204
```

Then configure Alleycat:

```toml
relay = "https://relay.inoteexpress.com"
```

Restart the Alleycat daemon after changing `relay`; reload is intentionally rejected for relay changes because the iroh endpoint is bound at startup.
