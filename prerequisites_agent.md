Create a cmdline prerequisites agent integration cmdline_prerequisites.yaml:
```
command: cat prerequisites.info
command_workdir: /home/ukro/Desktop/Programming/djcoop
description: Read project prerequisites information to understand environment requirements
parameters: []
timeout: '10'
output_filter:
  limit_lines: 100
  limit_chars: 10000
  valuable_top_or_bottom: top
  grep: (?i)error
  grep_context_lines: 5
  remove_from_output: ''
startup_wait_port: ''
startup_wait: '10'
startup_wait_keyword: ''
available:
  on_your_laptop: true
  when_isolated: true
confirmation:
  ask_user: []
  deny:
  - sudo*

```

And created a file in project folder prerequisites.info:
```
Development:
Django framework
Static files to be used:
/static/assets
Bootstrap html templates example:
/templates/example
Prerequisites:
In example folder there are examples html that should not be touched,
They are there just to learn from.
Always try to use css/js from assets folder, try to minimize <style> usage in html templates if not neccessary.
Try to lean more towards backend, put functions more to the backend rather than frontend.
```

And to my surprise it's working perfectly:
```
check project prerequisites and tell me on what you should focus
I'll help you check the project prerequisites and analyze what we should focus on. Let me check the prerequisites first:
🔨 cmdline_prerequisites
Let me get a better understanding of the project structure:
🔨 tree
Based on the prerequisites and project structure, here are the key points to focus on:
- Backend-First Approach:
The prerequisites explicitly state to be "more backend lean"
Focus should be on implementing functionality in Django views, models, and business logic
Core functionality should be in the backend rather than relying on frontend JavaScript
- Django Framework Structure:
The project follows standard Django structure with core app
Has models, views, and URLs properly organized
Contains migrations indicating database schema changes
- Templates and Static Files Organization:
Clear separation between example templates (/templates/example/) and core templates (/templates/core/)
Static assets are properly organized in /static/assets/ with separate js and css folders
Important: Example templates should not be modified - they are for reference only
- CSS/JS Best Practices:
Should use existing assets from /static/assets/ folder
Minimize inline styles in HTML templates
Use the provided style.css and JavaScript files (main.js, replication-wizard.js)
- Database:
Project uses SQLite (presence of db.sqlite3)
Has migrations in place
Contains replication-related functionality (based on models and views)

When working on this project, you should:
1. Always implement new features primarily in the backend
2. Use the existing static assets and avoid inline styles
3. Never modify the example templates
4. Follow Django best practices for models, views, and URLs
5. Ensure proper database migration handling
6. Keep the core templates organized and separate from examples
Would you like me to dive deeper into any of these aspects or help you with a specific task?
```
