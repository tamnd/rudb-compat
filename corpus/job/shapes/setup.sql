-- The tables every file in this directory runs over, a few rows each, shaped like JOB's.
-- They hold the things a semijoin reduction can get wrong: a NULL key, a child key with no parent,
-- a parent with no child, duplicate rows, an empty string, and titles whose UTF-8 order is not
-- their UTF-16 order.
CREATE TABLE title (id INTEGER, title VARCHAR, production_year INTEGER, kind_id INTEGER);
CREATE TABLE kind_type (id INTEGER, kind VARCHAR);
CREATE TABLE keyword (id INTEGER, keyword VARCHAR);
CREATE TABLE movie_keyword (id INTEGER, movie_id INTEGER, keyword_id INTEGER);
CREATE TABLE movie_info (id INTEGER, movie_id INTEGER, info VARCHAR, note VARCHAR);
CREATE TABLE company_name (id INTEGER, name VARCHAR, country_code VARCHAR);
CREATE TABLE movie_companies (id INTEGER, movie_id INTEGER, company_id INTEGER, note VARCHAR);
CREATE TABLE movie_link (id INTEGER, movie_id INTEGER, linked_movie_id INTEGER);
INSERT INTO kind_type VALUES (1, 'movie'), (2, 'episode'), (3, 'video game');
INSERT INTO title VALUES (1, 'Alpha', 1999, 1), (2, '', 2004, 1), (3, 'Zoë', 2010, 2), (4, 'ｚ wide', 2011, 1), (5, '😀 smile', 2012, 1), (6, 'Beta', NULL, 1), (7, 'Gamma', 1950, NULL), (8, 'Delta', 2001, 3), (9, 'Alpha', 1998, 1);
INSERT INTO keyword VALUES (1, 'sequel'), (2, 'character-name-in-title'), (3, 'murder'), (4, 'unused');
INSERT INTO movie_keyword VALUES (1, 1, 1), (2, 1, 1), (3, 2, 2), (4, 3, 1), (5, 4, 3), (6, 5, 1), (7, 99, 3), (8, NULL, 1), (9, 6, NULL), (10, 9, 2), (11, 8, 3);
INSERT INTO movie_info VALUES (1, 1, 'USA', NULL), (2, 2, 'Germany', '(festival)'), (3, 3, 'USA', '(TV)'), (4, 5, 'Japan', NULL), (5, 99, 'USA', NULL), (6, 4, 'USA', '(internet)'), (7, 8, 'Sweden', '(co-production)'), (8, 9, 'USA', '');
INSERT INTO company_name VALUES (1, 'Warner Bros.', '[us]'), (2, 'Studio Ghibli', '[jp]'), (3, 'Nordisk', '[se]'), (4, 'Nobody', NULL);
INSERT INTO movie_companies VALUES (1, 1, 1, '(presents)'), (2, 2, 1, NULL), (3, 3, 2, '(co-production)'), (4, 5, 2, '(presents)'), (5, 8, 3, NULL), (6, 9, 1, '(as Warner)'), (7, 99, 4, NULL), (8, 4, NULL, NULL);
INSERT INTO movie_link VALUES (1, 1, 9), (2, 9, 1), (3, 3, 5), (4, 5, 99), (5, 2, 2);
