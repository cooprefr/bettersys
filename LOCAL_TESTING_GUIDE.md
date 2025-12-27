# 🧪 BetterBot Local Testing Guide

**Status:** Ready to Test  
**Date:** November 16, 2025  

---

## ✅ Completed Setup

### Frontend Components Created (32 files)
- ✅ Configuration (7 files)
- ✅ TypeScript Types (2 files)
- ✅ Services (2 files)
- ✅ Stores (2 files)
- ✅ Utilities (1 file)
- ✅ Styles (1 file)
- ✅ Hooks (3 files)
- ✅ Effects (3 files)
- ✅ Auth Components (2 files)
- ✅ Terminal Components (3 files)
- ✅ Layout Components (2 files)
- ✅ App & Entry (4 files)

### Backend
- ✅ Rust trading engine running
- ✅ SQLite databases operational
- ✅ WebSocket ready
- ✅ REST API ready
- ✅ Default admin user created

---

## 🚀 Testing Steps

### Step 1: Start Backend

Open Terminal 1:
```bash
cd /Users/aryaman/betterbot/rust-backend
cargo run
```

**Expected Output:**
```
Compiling betterbot-backend v0.1.0
    Finished dev [unoptimized + debuginfo] target(s) in 5.26s
     Running `target/debug/betterbot`
🚀 Server listening on http://0.0.0.0:3000
```

**Leave this running!**

---

### Step 2: Start Frontend

Open Terminal 2:
```bash
cd /Users/aryaman/betterbot/frontend
npm run dev
```

**Expected Output:**
```
  VITE v5.0.0  ready in 300 ms

  ➜  Local:   http://localhost:5173/
  ➜  Network: http://192.168.x.x:5173/
  ➜  press h to show help
```

**Leave this running!**

---

### Step 3: Open Browser

1. **Open:** http://localhost:5173
2. **You should see:** Cyberpunk login screen with BETTERBOT logo

---

### Step 4: Login

**Default Credentials:**
- Username: `admin`
- Password: `admin123`

Click **AUTHENTICATE >**

---

### Step 5: Verify Terminal

Once logged in, you should see:

✅ **Terminal Header** (top)
- BETTERBOT logo with neon glow
- Signal stats (SIGNALS, HIGH CONF, AVG CONF)
- Latency indicator
- Green "CONNECTED" dot
- User info and LOGOUT button

✅ **Signal Feed** (center)
- "REAL-TIME SIGNAL STREAM" title
- Auto-scroll toggle button
- "SCANNING FOR SIGNALS..." message (if no signals yet)

✅ **Status Bar** (bottom)
- Keyboard shortcuts [F1-F4, ESC]
- Connection status with green dot

✅ **Visual Effects**
- CRT scanlines moving across screen
- Brutalist borders around components
- Indigo neon glow on text
- Smooth animations

---

### Step 6: Test Backend Connection

Open Terminal 3 (for testing):
```bash
# Test health endpoint
curl http://localhost:3000/health

# Expected: {"status":"ok","timestamp":"2025-11-16T..."}

# Test signals endpoint
curl http://localhost:3000/api/signals

# Expected: {"signals":[...],"total":0}  (or some signals if backend generated them)

# Test WebSocket
npm install -g wscat
wscat -c ws://localhost:3000/ws

# Expected: Connected (press Ctrl+C to exit)
```

---

### Step 7: Generate Test Signals

If you want to see signals appear in the terminal, the backend needs to detect them.

**Option A: Wait for real signals** (if DomeAPI/Polymarket are configured)

**Option B: Trigger manual signal generation** (if implemented)

**Option C: Check existing signals:**
```bash
curl http://localhost:3000/api/signals?limit=10
```

If backend has generated signals, they will appear in the terminal feed!

---

## ✅ Success Checklist

Check each item:

### Frontend
- [ ] Login screen appears with CRT effects
- [ ] Login successful with admin/admin123
- [ ] Terminal loads with header, feed, status bar
- [ ] WebSocket shows "CONNECTED" (green dot)
- [ ] No console errors in browser DevTools (F12)
- [ ] Hover effects work on cards/buttons
- [ ] Logout button works

### Backend
- [ ] Backend server running on port 3000
- [ ] Health endpoint responds
- [ ] Login API works (curl test)
- [ ] Signals API responds
- [ ] WebSocket accepts connections

### Integration
- [ ] Frontend connects to backend
- [ ] WebSocket latency shows <100ms
- [ ] Stats display (even if 0)
- [ ] No CORS errors
- [ ] No 404 errors in Network tab

---

## 🐛 Troubleshooting

### Issue: Frontend won't start
```bash
cd frontend
rm -rf node_modules package-lock.json
npm install
npm run dev
```

### Issue: Backend connection fails
Check backend is running:
```bash
curl http://localhost:3000/health
```

If fails, restart backend.

### Issue: Login fails
Check backend logs. Default user might not be created.

Create user manually:
```bash
# (You may need to implement a create-user script)
```

### Issue: WebSocket won't connect
- Check backend logs for WebSocket errors
- Check browser console for connection errors
- Verify backend is running on port 3000
- Check firewall isn't blocking connections

### Issue: No signals appearing
This is normal if:
- Backend hasn't detected any signals yet
- No markets meet signal criteria
- DomeAPI/Polymarket aren't configured

To verify WebSocket is working:
- Check "CONNECTED" status (green dot)
- Latency should show a number (even if 0.0ms)

---

## 🎨 Visual Verification

### You should see:
✅ Black background (`#020208`)  
✅ CRT scanlines moving vertically  
✅ Indigo neon glow on BETTERBOT text  
✅ Brutal borders (indigo, 1px solid)  
✅ Orange accents on $BETTER  
✅ Matrix green on CONNECTED  
✅ Smooth hover effects  
✅ Monospace font (Space Mono)  

### You should NOT see:
❌ White background  
❌ Default browser fonts  
❌ Missing borders  
❌ Broken layout  
❌ Console errors  

---

## 📊 Performance Check

### Frontend
- Initial load: <2s
- Page size: <500KB
- No memory leaks
- Smooth animations (60fps)

### Backend
- Health check: <10ms
- Login: <100ms
- Signals API: <50ms
- WebSocket latency: <5ms

---

## 🔥 Next Steps

Once everything works locally:

1. ✅ **Verify all features work**
2. ✅ **Check for bugs**
3. ✅ **Test on different browsers**
4. ✅ **Prepare for production deployment**

---

## 📞 Quick Commands

```bash
# Start backend
cd rust-backend && cargo run

# Start frontend
cd frontend && npm run dev

# Test health
curl http://localhost:3000/health

# Test login
curl -X POST http://localhost:3000/api/auth/login \
  -H "Content-Type: application/json" \
  -d '{"username":"admin","password":"admin123"}'

# Test WebSocket
wscat -c ws://localhost:3000/ws

# Open browser
open http://localhost:5173
```

---

**Ready to test? Start the backend first, then the frontend, then open your browser!** 🚀

*"The terminal is complete. Time to see it in action!"*
