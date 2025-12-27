#!/bin/bash
# Quick test of Polymarket API fetch

curl -s "https://gamma-api.polymarket.com/events?closed=false&limit=2" | jq '.[0] | {title, volume, liquidity, volume_type: (.volume | type), liquidity_type: (.liquidity | type)}'
