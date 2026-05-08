# Demo Video Recording Guide

This guide helps you create a professional demo video of AgentFlow in action.

## Recording Tools

### macOS
- **Built-in**: QuickTime Player (File > New Screen Recording)
- **Recommended**: [Loom](https://www.loom.com/) - Free, easy editing
- **Pro**: [ScreenFlow](https://www.telestream.net/screenflow/) - Advanced editing

### Linux
- **Simple**: `simplescreenrecorder`
  ```bash
  sudo apt install simplescreenrecorder  # Ubuntu/Debian
  ```
- **Professional**: OBS Studio
  ```bash
  sudo apt install obs-studio
  ```

### Windows
- **Built-in**: Windows Game Bar (`Win + G`)
- **Free**: OBS Studio (https://obsproject.com/)
- **Pro**: Camtasia

## Demo Script (5-7 minutes)

### Scene 1: Introduction (30 seconds)
**Show**: AgentFlow README on GitHub

**Narration**:
> "AgentFlow is an autonomous AI development team that takes GitHub issues and turns them into working code with pull requests - completely autonomously. Let me show you how it works."

### Scene 2: Prerequisites Check (45 seconds)
**Show**: Terminal running setup checker

```bash
cd AgentFlow
./scripts/check_setup.sh
```

**Narration**:
> "First, we verify our environment. AgentFlow needs Rust, Node.js, and the Claude Code CLI. Our setup checker confirms everything is ready."

### Scene 3: Create Target Repository (1 minute)
**Show**: Terminal creating a new repository

```bash
# Create repository
gh repo create demo-calculator --public --clone
cd demo-calculator

# Initialize
echo "# Calculator App" > README.md
git add . && git commit -m "Initial commit" && git push

# Create issues
gh issue create \
  --title "Build a calculator web app" \
  --body "Create a simple calculator with HTML/CSS/JavaScript. Support basic operations: add, subtract, multiply, divide."

gh issue list
```

**Narration**:
> "I'm creating a simple repository with a calculator issue. This is what our AI team will autonomously build."

### Scene 4: Configure AgentFlow (30 seconds)
**Show**: Editing `.env` file (blur API keys!)

```bash
cd ../AgentFlow
nano .env
```

**Narration**:
> "I configure AgentFlow with my API keys and point it to the repository we just created."

### Scene 5: Start Orchestration (30 seconds)
**Show**: Terminal running AgentFlow

```bash
cargo run --bin real_test
```

**Narration**:
> "Now I start the orchestration. AgentFlow's NEXUS agent will discover the issue and assign it to a FORGE worker."

### Scene 6: Watch the Logs (1-2 minutes)
**Show**: Split screen - logs + worker log

Terminal 1:
```bash
cargo run --bin real_test
```

Terminal 2:
```bash
# Wait for worker to start, then show logs
tail -f ~/.agentflow/workspaces/your-username-demo-calculator/forge/workers/forge-1/worker.log
```

**Narration**:
> "The NEXUS agent has assigned the work. Now watch as the FORGE agent autonomously:
> - Creates an isolated worktree
> - Spawns Claude Code
> - Implements the calculator
> - Creates tests
> - Opens a pull request
> 
> All without any human intervention."

**Optional**: Speed up this section 2-4x in editing.

### Scene 7: Inspect Results (1-2 minutes)
**Show**: Terminal and browser split screen

```bash
# Check STATUS.json
cat ~/.agentflow/workspaces/your-username-demo-calculator/worktrees/forge-1/STATUS.json

# View the generated files
cd ~/.agentflow/workspaces/your-username-demo-calculator/worktrees/forge-1
ls -la

# Show the code
cat index.html
cat calculator.js
```

**Narration**:
> "The agent has completed the work. Let's see what it created."

**Show**: Browser viewing the pull request on GitHub

```bash
gh pr list --repo your-username/demo-calculator
gh pr view 1 --repo your-username/demo-calculator
```

**Narration**:
> "And here's the pull request the agent opened. It includes all the code, tests, and documentation."

### Scene 8: Test the App (30 seconds)
**Show**: Browser with the calculator running

```bash
# In the worktree directory
python3 -m http.server 8000
```

Open `http://localhost:8000` in browser and demo the calculator.

**Narration**:
> "And here's the working calculator - built entirely by AI agents."

### Scene 9: Conclusion (30 seconds)
**Show**: AgentFlow architecture diagram or README

**Narration**:
> "That's AgentFlow - an autonomous development team that can handle your GitHub issues from start to finish. Check out the repository to learn more and try it yourself."

**Show**: GitHub repo URL: `github.com/The-AgenticFlow/AgentFlow`

## Recording Tips

### Before Recording
1. **Clean your desktop** - Close unnecessary windows
2. **Hide sensitive info** - Clear bash history, use test API keys
3. **Increase terminal font** - Use 18-20pt for readability
4. **Use a clean terminal theme** - Avoid distracting colors
5. **Test the full flow** - Do a dry run before recording

### Terminal Setup
```bash
# Increase font size (adjust for your terminal)
# VS Code: Ctrl/Cmd + = (multiple times)
# iTerm2: Cmd + + 
# GNOME Terminal: View > Zoom In

# Use a clean prompt
export PS1='\[\033[01;32m\]\u@agentflow\[\033[00m\]:\[\033[01;34m\]\w\[\033[00m\]\$ '

# Clear history
clear
```

### During Recording
- **Speak clearly and steadily** - Don't rush
- **Explain what's happening** - Assume viewers know nothing
- **Use pauses** - Give viewers time to read output
- **Highlight key moments** - "Notice how the agent creates a worktree..."
- **Show, don't just tell** - Scroll through files, show code

### After Recording
1. **Add title cards** - "AgentFlow Demo", "Prerequisites", "Results", etc.
2. **Annotate key moments** - Add text overlays for important logs
3. **Speed up long waits** - 2-4x speed for the 5-15 min build process
4. **Add background music** - Low-volume, non-distracting (check licensing)
5. **Create chapters** - YouTube allows chapter markers
6. **Add end screen** - GitHub repo link, documentation links

## Video Specs

**Recommended settings:**
- **Resolution**: 1920x1080 (1080p)
- **Frame rate**: 30fps (or 60fps for smoother)
- **Format**: MP4 (H.264)
- **Bitrate**: 8-12 Mbps

## Example Timeline

```
00:00 - Introduction
00:30 - Prerequisites check
01:15 - Create target repository
02:15 - Configure AgentFlow  
02:45 - Start orchestration
03:15 - Watch the AI work (sped up 4x)
05:00 - Inspect generated code
06:00 - View pull request
06:30 - Test the working app
07:00 - Conclusion
```

## Publishing

### YouTube
1. Title: "AgentFlow: Autonomous AI Development Team Demo"
2. Description: Include GitHub repo link, timestamps
3. Tags: AI, automation, GitHub, development, autonomous agents, Claude
4. Thumbnail: Screenshot of terminal + "AI Team Builds a Calculator"

### GitHub
1. Upload to `assets/demo.mp4` or link YouTube
2. Add to README: `[![Demo Video](thumbnail.png)](https://youtube.com/...)`

### Social Media
- Twitter/X: Short 1-min clip showing the key moment
- LinkedIn: Professional angle - "AI automation in software development"
- Reddit: r/programming, r/MachineLearning (check subreddit rules)

## Optional: Animated GIF

Create a shorter GIF for README (30-60 seconds max):

```bash
# Install ffmpeg if needed
# Ubuntu: sudo apt install ffmpeg
# macOS: brew install ffmpeg

# Convert video to GIF
ffmpeg -i demo.mp4 -vf "fps=10,scale=1280:-1:flags=lanczos" -t 60 demo.gif

# Optimize size
ffmpeg -i demo.gif -vf "fps=10,scale=800:-1:flags=lanczos" -loop 0 demo_optimized.gif
```

Show:
1. Issue creation
2. AgentFlow starting
3. Logs showing work assignment
4. Pull request opened
5. Final working calculator

## Need Help?

If you need help creating a demo video:
- Open a discussion: https://github.com/The-AgenticFlow/AgentFlow/discussions
- We may feature community demos in the README!

---

**Happy Recording! 🎥**
