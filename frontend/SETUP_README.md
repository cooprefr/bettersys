# BetterBot Terminal Frontend

## 🚀 Quick Start

### Prerequisites

1. **Install Node.js 18+**
```bash
# On macOS (using Homebrew)
brew install node

# Or download from: https://nodejs.org/
```

2. **Verify installation**
```bash
node --version  # Should show v18.x or higher
npm --version   # Should show 9.x or higher
```

### Installation

```bash
# Navigate to frontend directory
cd /Users/aryaman/betterbot/frontend

# Install dependencies
npm install

# This will install:
# - React + TypeScript
# - Vite (build tool)
# - TailwindCSS (styling)
# - Zustand (state management)
# - Framer Motion (animations)
# - Chart.js (charts)
# - Three.js (3D effects)
# - date-fns (date formatting)
```

### Development

```bash
# Start development server
npm run dev

# Frontend will be available at: http://localhost:5173
# Hot module replacement (HMR) enabled for instant updates
```

### Building for Production

```bash
# Build optimized production bundle
npm run build

# Preview production build
npm run preview
```

---

## 📁 Project Structure

```
frontend/
├── public/              # Static assets
├── src/
│   ├── components/      # React components
│   │   ├── terminal/    # Signal feed, cards, charts
│   │   ├── effects/     # CRT, scanlines, particles
│   │   ├── auth/        # Login, auth guard
│   │   └── layout/      # App shell, sidebar, status
│   ├── hooks/           # Custom React hooks
│   ├── services/        # API client, WebSocket
│   ├── stores/          # Zustand state management
│   ├── types/           # TypeScript definitions
│   ├── styles/          # Global CSS
│   ├── utils/           # Utilities & formatters
│   ├── App.tsx          # Root component
│   └── main.tsx         # Entry point
├── index.html           # HTML template
├── package.json         # Dependencies
├── vite.config.ts       # Vite configuration
├── tailwind.config.js   # TailwindCSS config
└── tsconfig.json        # TypeScript config
```

---

## 🎨 Design System

### Colors
- **Background:** `#020208` (Void Black)
- **Terminal:** `#050A0E` (Deep Space)
- **Primary:** `#6366f1` (Indigo Neon)
- **Accent:** `#F97316` (Orange Alert)
- **Success:** `#10b981` (Matrix Green)
- **Error:** `#ef4444` (Critical Red)

### Fonts
- **Primary:** Space Mono (monospace)
- **Terminal:** VT323 (retro)
- **Headers:** Orbitron (futuristic)

### Effects
- CRT scanlines
- Phosphor glow
- Glitch animations
- Data particles
- Signal pulse waves

---

## 🔌 Backend Connection

The frontend connects to the Rust backend via:

1. **REST API** (`http://localhost:3000/api`)
   - Authentication
   - Signal queries
   - Stats retrieval

2. **WebSocket** (`ws://localhost:3000/ws`)
   - Real-time signal feed
   - Live updates
   - Sub-5ms latency

---

## 🧪 Testing Locally

### Start Backend
```bash
cd /Users/aryaman/betterbot/rust-backend
cargo run
```

### Start Frontend
```bash
cd /Users/aryaman/betterbot/frontend
npm run dev
```

### Access Terminal
Open browser: `http://localhost:5173`

**Default Login:**
- Username: `admin`
- Password: `admin123`

---

## 📦 Component Overview

### Already Created:
- ✅ Configuration files (package.json, vite.config.ts, etc.)
- ✅ TypeScript types (Signal, Auth, API)
- ✅ Services (API client, WebSocket client)
- ✅ Stores (Signal store, Auth store)
- ✅ Utilities (Formatters, colors, icons)
- ✅ Global styles (CRT effects, animations)

### To Be Created (Next Steps):
- 🔄 React Components (Auth, Terminal, Effects)
- 🔄 Main App.tsx and index.html
- 🔄 Hooks (useWebSocket, useSignals, useAuth)

---

## 🚀 Next Steps

1. **Install Node.js** (if not installed)
2. **Run `npm install`** in frontend directory
3. **I'll create remaining React components**
4. **Test locally with backend running**
5. **Deploy to production**

---

## 💡 Tips

- Press `Ctrl+C` to stop dev server
- Edit files and see changes instantly (HMR)
- Check browser console for errors
- WebSocket status shown in bottom-right
- Use Chrome DevTools for debugging

---

**Ready to create the remaining components?** Let me know when Node.js is installed!
